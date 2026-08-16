//! Integration tests for the multi-tenant security invariants: authentication,
//! per-course membership scoping, enrollment-token isolation, and provisioning.
//!
//! Requires a Postgres database. Set `HERMIONE_TEST_DATABASE_URL` (defaults to
//! `postgres://hermione:hermione@localhost:5432/hermione_test`). Tables are
//! shared across tests; each test namespaces its rows with a random suffix.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use hermione_migration::{Migrator, MigratorTrait};
use sea_orm::Database;
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::Auth;
use crate::state::{AppState, Hub, MsgHub};
use crate::{http, tenancy};

fn test_url() -> String {
    std::env::var("HERMIONE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hermione:hermione@localhost:5432/hermione_test".into())
}

/// Runs migrations exactly once (on a throwaway runtime), before any test
/// connects. Each test then opens its own pool — a sqlx pool is bound to the
/// runtime that created it, so it must not be shared across `#[tokio::test]`s.
fn ensure_schema() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Run on a dedicated thread so we can build a runtime without nesting
        // inside the test's own `#[tokio::test]` runtime.
        std::thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let db = Database::connect(test_url())
                    .await
                    .expect("connect HERMIONE_TEST_DATABASE_URL");
                Migrator::up(&db, None).await.expect("run migrations");
            });
        })
        .join()
        .expect("schema init thread");
    });
}

/// Builds app state with authentication enforced (an admin exists, so not open
/// dev) and a known super-admin token. Each test gets its own connection pool.
async fn app() -> (AppState, Router) {
    app_with_identity(crate::identity::Identity::disabled()).await
}

async fn app_with_identity(identity: crate::identity::Identity) -> (AppState, Router) {
    ensure_schema();
    let state = AppState {
        db: Database::connect(test_url())
            .await
            .expect("connect HERMIONE_TEST_DATABASE_URL"),
        hub: Hub::default(),
        msg_hub: MsgHub::default(),
        auth: Auth::new(),
        identity,
        admin_token: Some("admintok".to_string()),
        open_dev: Arc::new(AtomicBool::new(false)),
        assistant: crate::assistant::Assistant::new(None, None),
        assistant_default_model: "claude-opus-4-8".to_string(),
        github_token: None,
        github_allowed_owners: Vec::new(),
        github_app: None,
    };
    let router = http::router(state.clone());
    (state, router)
}

fn rnd() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_string()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Logs in and returns the `hermione_session=...` cookie, if successful.
async fn login(app: &Router, user: &str, pass: &str) -> Option<String> {
    let req = Request::post("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"username":"{user}","password":"{pass}"}}"#
        )))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    if resp.status() != StatusCode::NO_CONTENT {
        return None;
    }
    let cookie = resp.headers().get(header::SET_COOKIE)?.to_str().ok()?;
    Some(cookie.split(';').next()?.to_string())
}

async fn get_with_cookie(app: &Router, uri: &str, cookie: &str) -> axum::response::Response {
    let req = Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

async fn post_json_with_cookie(
    app: &Router,
    uri: &str,
    cookie: &str,
    body: &str,
) -> axum::response::Response {
    req_with_cookie(app, "POST", uri, cookie, Some(body)).await
}

async fn req_with_cookie(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    let body = match body {
        Some(b) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn unauthenticated_api_is_rejected() {
    let (_state, app) = app().await;
    let resp = app
        .oneshot(
            Request::get("/api/overview?course=default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let (state, app) = app().await;
    let s = rnd();
    tenancy::create_admin(&state.db, &format!("a{s}"), "correct-pw")
        .await
        .unwrap();
    assert!(login(&app, &format!("a{s}"), "correct-pw").await.is_some());
    assert!(login(&app, &format!("a{s}"), "wrong-pw").await.is_none());
}

#[tokio::test]
async fn membership_scopes_dashboard_access() {
    let (state, app) = app().await;
    let s = rnd();
    let mine = tenancy::create_course(&state.db, &format!("mine{s}"), "Mine", None)
        .await
        .unwrap();
    tenancy::create_course(&state.db, &format!("other{s}"), "Other", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("a{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, mine.id)
        .await
        .unwrap();

    let cookie = login(&app, &format!("a{s}"), "pw").await.expect("login");

    // Member course: allowed.
    let resp = get_with_cookie(&app, &format!("/api/overview?course=mine{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Non-member course: forbidden.
    let resp = get_with_cookie(&app, &format!("/api/overview?course=other{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Unknown course: not found.
    let resp = get_with_cookie(&app, &format!("/api/overview?course=nope{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn enrollment_token_isolates_tenants() {
    let (state, app) = app().await;
    let s = rnd();
    let x = tenancy::create_course(&state.db, &format!("x{s}"), "X", None)
        .await
        .unwrap();
    let y = tenancy::create_course(&state.db, &format!("y{s}"), "Y", None)
        .await
        .unwrap();
    let student = format!("stud{s}");

    // Ingest activity into X using X's enrollment token.
    let now = chrono::Utc::now().timestamp_millis();
    let payload =
        format!(r#"[{{"student":"{student}","path":"/a.py","kind":"focus","atUnixMs":{now}}}]"#);
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", x.enrollment_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // An admin of Y must NOT see X's student.
    let admin_y = tenancy::create_admin(&state.db, &format!("ay{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin_y.id, y.id)
        .await
        .unwrap();
    let cookie_y = login(&app, &format!("ay{s}"), "pw").await.unwrap();
    let resp = get_with_cookie(
        &app,
        &format!("/api/students/activity?course=y{s}"),
        &cookie_y,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_y = body_string(resp).await;
    assert!(
        !body_y.contains(&student),
        "Y admin must not see X's student, got: {body_y}"
    );

    // An admin of X must see X's student.
    let admin_x = tenancy::create_admin(&state.db, &format!("ax{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin_x.id, x.id)
        .await
        .unwrap();
    let cookie_x = login(&app, &format!("ax{s}"), "pw").await.unwrap();
    let resp = get_with_cookie(
        &app,
        &format!("/api/students/activity?course=x{s}"),
        &cookie_x,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_x = body_string(resp).await;
    assert!(
        body_x.contains(&student),
        "X admin should see X's student, got: {body_x}"
    );
}

#[tokio::test]
async fn ingest_requires_valid_enrollment_token() {
    let (_state, app) = app().await;
    let payload = r#"[{"student":"x","path":"/a.py","kind":"focus","atUnixMs":0}]"#;

    // No token.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Bad token.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(header::AUTHORIZATION, "Bearer not-a-real-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn provisioning_requires_admin_token() {
    let (_state, app) = app().await;
    let s = rnd();
    let body = format!(r#"{{"slug":"prov{s}","name":"Prov"}}"#);

    // Missing token.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/admin/courses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Correct token.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/admin/courses")
                .header(header::AUTHORIZATION, "Bearer admintok")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn teacher_creates_course_and_is_enrolled() {
    let (state, app) = app().await;
    let s = rnd();
    tenancy::create_admin(&state.db, &format!("t{s}"), "pw")
        .await
        .unwrap();
    let cookie = login(&app, &format!("t{s}"), "pw").await.expect("login");

    // Create a course by linking a repo, letting the server derive the slug.
    let repo = format!("https://github.com/org/course{s}.git");
    // seedExercises:false keeps the test hermetic (no live GitHub call).
    let body = format!(r#"{{"name":"My Course {s}","repoUrl":"{repo}","seedExercises":false}}"#);
    let resp = post_json_with_cookie(&app, "/api/courses", &cookie, &body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let slug = created["slug"].as_str().unwrap().to_string();
    assert_eq!(slug, format!("course{s}"), "slug derived from repo");
    assert_eq!(created["repoUrl"], repo);
    assert!(
        created["enrollmentToken"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "enrollment token returned"
    );

    // The creator is now a member, so the course is scoped to them...
    let resp = get_with_cookie(&app, "/api/courses", &cookie).await;
    let listed = body_string(resp).await;
    assert!(
        listed.contains(&slug),
        "new course visible to creator: {listed}"
    );

    // ...and its data endpoints are accessible.
    let resp = get_with_cookie(&app, &format!("/api/overview?course={slug}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Re-creating the same slug is a conflict.
    let dup = format!(r#"{{"slug":"{slug}","name":"dup"}}"#);
    let resp = post_json_with_cookie(&app, "/api/courses", &cookie, &dup).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // A course needs at least a slug, name, or repo.
    let resp = post_json_with_cookie(&app, "/api/courses", &cookie, "{}").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn teacher_manages_course_settings() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("mgmt{s}"), "Mgmt", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("owner{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("owner{s}"), "pw").await.unwrap();
    let uri = format!("/api/courses/mgmt{s}");

    // Detail carries the enrollment token and the member list.
    let resp = get_with_cookie(&app, &uri, &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let original_token = detail["enrollmentToken"].as_str().unwrap().to_string();
    assert!(detail["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == &format!("owner{s}")));

    // Rename + link a repo.
    let patch = r#"{"name":"Renamed","repoUrl":"https://github.com/org/x"}"#;
    let resp = req_with_cookie(&app, "PATCH", &uri, &cookie, Some(patch)).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let updated = tenancy::course_by_slug(&state.db, &format!("mgmt{s}"))
        .await
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(
        updated.repo_url.as_deref(),
        Some("https://github.com/org/x")
    );

    // Clearing the repo (explicit null) unlinks it.
    let resp = req_with_cookie(&app, "PATCH", &uri, &cookie, Some(r#"{"repoUrl":null}"#)).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cleared = tenancy::course_by_slug(&state.db, &format!("mgmt{s}"))
        .await
        .unwrap();
    assert_eq!(cleared.repo_url, None);

    // Rotating the token changes it.
    let resp = req_with_cookie(
        &app,
        "POST",
        &format!("/api/courses/mgmt{s}/rotate-token"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rotated: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_ne!(rotated["enrollmentToken"].as_str().unwrap(), original_token);

    // A non-member cannot touch it.
    tenancy::create_admin(&state.db, &format!("outsider{s}"), "pw")
        .await
        .unwrap();
    let outsider = login(&app, &format!("outsider{s}"), "pw").await.unwrap();
    let resp = req_with_cookie(&app, "PATCH", &uri, &outsider, Some(r#"{"name":"nope"}"#)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn course_profile_is_set_on_create_and_editable() {
    let (state, app) = app().await;
    let s = rnd();
    tenancy::create_admin(&state.db, &format!("p{s}"), "pw")
        .await
        .unwrap();
    let cookie = login(&app, &format!("p{s}"), "pw").await.unwrap();

    // Create with profile fields.
    let body = format!(
        r#"{{"slug":"prof{s}","name":"Prof","seedExercises":false,
             "description":"All about pointers","term":"Fall 2026",
             "institution":"Acme U","level":"Beginner"}}"#
    );
    let resp = post_json_with_cookie(&app, "/api/courses", &cookie, &body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let uri = format!("/api/courses/prof{s}");
    let detail: serde_json::Value =
        serde_json::from_str(&body_string(get_with_cookie(&app, &uri, &cookie).await).await)
            .unwrap();
    assert_eq!(detail["description"], "All about pointers");
    assert_eq!(detail["term"], "Fall 2026");
    assert_eq!(detail["institution"], "Acme U");
    assert_eq!(detail["level"], "Beginner");

    // Edit one field and clear another (explicit null).
    let patch = r#"{"level":"Advanced","term":null}"#;
    let resp = req_with_cookie(&app, "PATCH", &uri, &cookie, Some(patch)).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let detail: serde_json::Value =
        serde_json::from_str(&body_string(get_with_cookie(&app, &uri, &cookie).await).await)
            .unwrap();
    assert_eq!(detail["level"], "Advanced");
    assert!(detail["term"].is_null(), "term cleared");
    assert_eq!(
        detail["description"], "All about pointers",
        "untouched field kept"
    );

    // The switcher list carries the description (for the tooltip).
    let list = body_string(get_with_cookie(&app, "/api/courses", &cookie).await).await;
    assert!(
        list.contains("All about pointers"),
        "description in list: {list}"
    );
}

#[tokio::test]
async fn teacher_manages_co_teachers() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("team{s}"), "Team", None)
        .await
        .unwrap();
    let owner = tenancy::create_admin(&state.db, &format!("o{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, owner.id, course.id)
        .await
        .unwrap();
    tenancy::create_admin(&state.db, &format!("colleague{s}"), "pw")
        .await
        .unwrap();
    let cookie = login(&app, &format!("o{s}"), "pw").await.unwrap();
    let members_uri = format!("/api/courses/team{s}/members");

    // Add an existing admin as a co-teacher.
    let body = format!(r#"{{"username":"colleague{s}"}}"#);
    let resp = post_json_with_cookie(&app, &members_uri, &cookie, &body).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // They can now see the course.
    let colleague = login(&app, &format!("colleague{s}"), "pw").await.unwrap();
    let resp = get_with_cookie(&app, &format!("/api/overview?course=team{s}"), &colleague).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Adding an unknown user is a 404.
    let resp = post_json_with_cookie(&app, &members_uri, &cookie, r#"{"username":"ghost"}"#).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Remove the co-teacher.
    let resp = req_with_cookie(
        &app,
        "DELETE",
        &format!("/api/courses/team{s}/members/colleague{s}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = get_with_cookie(&app, &format!("/api/overview?course=team{s}"), &colleague).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Removing an admin who isn't a member is a 404 (not a last-member conflict).
    let resp = req_with_cookie(
        &app,
        "DELETE",
        &format!("/api/courses/team{s}/members/colleague{s}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // The last remaining member cannot be removed.
    let resp = req_with_cookie(
        &app,
        "DELETE",
        &format!("/api/courses/team{s}/members/o{s}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn archived_courses_leave_the_active_list() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("arch{s}"), "Arch", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("aa{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("aa{s}"), "pw").await.unwrap();

    // Archive it.
    let resp = req_with_cookie(
        &app,
        "PATCH",
        &format!("/api/courses/arch{s}"),
        &cookie,
        Some(r#"{"archived":true}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Gone from the active list, present in the archived list.
    let active = body_string(get_with_cookie(&app, "/api/courses", &cookie).await).await;
    assert!(
        !active.contains(&format!("arch{s}")),
        "archived hidden: {active}"
    );
    let archived =
        body_string(get_with_cookie(&app, "/api/courses?archived=1", &cookie).await).await;
    assert!(
        archived.contains(&format!("arch{s}")),
        "archived listed: {archived}"
    );

    // Restore it.
    let resp = req_with_cookie(
        &app,
        "PATCH",
        &format!("/api/courses/arch{s}"),
        &cookie,
        Some(r#"{"archived":false}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let active = body_string(get_with_cookie(&app, "/api/courses", &cookie).await).await;
    assert!(
        active.contains(&format!("arch{s}")),
        "restored to active: {active}"
    );
}

#[tokio::test]
async fn course_can_be_created_from_name_alone() {
    let (state, app) = app().await;
    let s = rnd();
    tenancy::create_admin(&state.db, &format!("n{s}"), "pw")
        .await
        .unwrap();
    let cookie = login(&app, &format!("n{s}"), "pw").await.unwrap();

    // Name only (no slug, no repo): the slug is derived from the name.
    let body = format!(r#"{{"name":"Intro Rust {s}"}}"#);
    let resp = post_json_with_cookie(&app, "/api/courses", &cookie, &body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(created["slug"], format!("intro-rust-{s}"));
}

#[tokio::test]
async fn course_creation_requires_login() {
    let (_state, app) = app().await;
    let resp = app
        .oneshot(
            Request::post("/api/courses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"slug":"nope","name":"Nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn defined_exercises_drive_overview_order() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("ord{s}"), "Ord", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("ad{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("ad{s}"), "pw").await.unwrap();

    // Define exercises with explicit positions (intentionally not slug order).
    let define = format!(
        r#"{{"course":"ord{s}","exercises":[{{"slug":"two","title":"Two","position":1}},{{"slug":"one","title":"One","position":0}},{{"slug":"three","title":"Three","position":2}}]}}"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/exercises")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(define))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Activity on "one" only; "three" stays defined-but-empty.
    let now = chrono::Utc::now().timestamp_millis();
    let ev = format!(
        r#"[{{"student":"st{s}","path":"/x.py","exercise":"one","kind":"focus","atUnixMs":{now}}}]"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", course.enrollment_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(ev))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = get_with_cookie(&app, &format!("/api/overview?course=ord{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let groups = v["exercises"].as_array().unwrap();
    let slugs: Vec<&str> = groups
        .iter()
        .map(|g| g["exercise"].as_str().unwrap())
        .collect();
    assert_eq!(
        &slugs[..3],
        &["one", "two", "three"],
        "defined order honored"
    );

    let three = groups.iter().find(|g| g["exercise"] == "three").unwrap();
    assert_eq!(
        three["stats"]["total"], 0,
        "defined-but-empty exercise shown"
    );
}

#[tokio::test]
async fn exercises_replace_sets_the_exact_list() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("ex{s}"), "Ex", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("ex{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("ex{s}"), "pw").await.unwrap();

    let slugs = |body: String| async move {
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|e| e["slug"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    // Upsert (no replace): one, two, three.
    let define = format!(
        r#"{{"course":"ex{s}","exercises":[{{"slug":"one"}},{{"slug":"two"}},{{"slug":"three"}}]}}"#
    );
    let resp = post_json_with_cookie(&app, "/api/exercises", &cookie, &define).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Replace with exactly [two, four]: one and three are removed, four added.
    let replace = format!(
        r#"{{"course":"ex{s}","replace":true,"exercises":[{{"slug":"two","title":"Two"}},{{"slug":"four"}}]}}"#
    );
    let resp = post_json_with_cookie(&app, "/api/exercises", &cookie, &replace).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = get_with_cookie(&app, &format!("/api/exercises?course=ex{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let got = slugs(body_string(resp).await).await;
    assert_eq!(got, ["two", "four"], "replace set the exact ordered list");
}

#[tokio::test]
async fn enforced_identity_required_and_trusted() {
    use crate::identity::{Identity, Provider, ProviderKind};
    let provider = Provider {
        name: "github".into(),
        kind: ProviderKind::Github,
        issuer: None,
        client_id: "client".into(),
        client_secret: None,
        scopes: None,
    };
    let identity = Identity::for_test("identity-secret", vec![provider]);
    let hermione_token = identity
        .issue("github:trusted")
        .expect("issue identity token");

    let (state, app) = app_with_identity(identity).await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("idc{s}"), "ID", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("ida{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let event = |student: &str| {
        format!(r#"[{{"student":"{student}","path":"/a.py","kind":"focus","atUnixMs":{now}}}]"#)
    };

    // Without a verified identity token, ingest is rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", course.enrollment_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(event("self-claimed")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // With a valid token, the trusted identity overrides the self-asserted one.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", course.enrollment_token),
                )
                .header("x-hermione-identity", &hermione_token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(event("self-claimed")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let cookie = login(&app, &format!("ida{s}"), "pw").await.unwrap();
    let resp = get_with_cookie(
        &app,
        &format!("/api/students/activity?course=idc{s}"),
        &cookie,
    )
    .await;
    let body = body_string(resp).await;
    assert!(
        body.contains("github:trusted"),
        "trusted identity stored: {body}"
    );
    assert!(
        !body.contains("self-claimed"),
        "self-asserted name ignored: {body}"
    );
}

/// Edit events must survive ingest and reach the overview, and a student who
/// has been on an exercise a long time without typing must read differently
/// from one who is still working.
#[tokio::test]
async fn edit_activity_separates_working_from_stuck() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("ed{s}"), "Ed", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("ed{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("ed{s}"), "pw").await.unwrap();

    // Both students have been on the same exercise for ~20 minutes. The only
    // difference is that one of them is still typing.
    let now = chrono::Utc::now().timestamp_millis();
    let start = now - 20 * 60 * 1000;
    let mut events = Vec::new();
    for who in [format!("busy{s}"), format!("idle{s}")] {
        // A heartbeat every minute keeps time-on-exercise accumulating for both.
        for m in 0..21 {
            events.push(format!(
                r#"{{"student":"{who}","path":"/a.py","exercise":"one","kind":"heartbeat","atUnixMs":{}}}"#,
                start + m * 60 * 1000
            ));
        }
    }
    // The busy student typed just now; the stuck one last typed 15 minutes ago.
    events.push(format!(
        r#"{{"student":"busy{s}","path":"/a.py","exercise":"one","kind":"edit","edits":37,"line":12,"atUnixMs":{now}}}"#
    ));
    events.push(format!(
        r#"{{"student":"idle{s}","path":"/a.py","exercise":"one","kind":"edit","edits":4,"line":3,"atUnixMs":{}}}"#,
        now - 15 * 60 * 1000
    ));
    let payload = format!("[{}]", events.join(","));

    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", course.enrollment_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = get_with_cookie(&app, &format!("/api/overview?course=ed{s}"), &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let students: Vec<&serde_json::Value> = v["exercises"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["students"].as_array().unwrap())
        .collect();

    let busy = students
        .iter()
        .find(|st| st["student"] == format!("busy{s}"))
        .expect("busy student in overview");
    let idle = students
        .iter()
        .find(|st| st["student"] == format!("idle{s}"))
        .expect("idle student in overview");

    assert_eq!(busy["editsRecent"], 37, "recent edits surfaced");
    assert_eq!(idle["editsRecent"], 0, "old edits fall outside the window");
    assert!(
        busy["lastEditUnixMs"].is_i64() && idle["lastEditUnixMs"].is_i64(),
        "last edit reported for both"
    );

    let reasons = |st: &serde_json::Value| {
        st["struggleReasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().starts_with("no edits for"))
    };
    assert!(!reasons(busy), "a typing student is not stalled");
    assert!(reasons(idle), "a silent student is stalled: {idle}");
}

/// An edit flushed after the student has already moved on must not drag the
/// board back to the file they left, and must not count as typing on the new
/// exercise.
#[tokio::test]
async fn a_late_edit_does_not_follow_the_student_to_the_next_exercise() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("lt{s}"), "Lt", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("lt{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("lt{s}"), "pw").await.unwrap();

    // Typed in "one", switched to "two" a second later, and the edit for "one"
    // only reached us after the switch — stamped when the typing happened.
    let now = chrono::Utc::now().timestamp_millis();
    let student = format!("sw{s}");
    let payload = format!(
        r#"[{{"student":"{student}","path":"/one.py","exercise":"one","kind":"focus","atUnixMs":{}}},
            {{"student":"{student}","path":"/two.py","exercise":"two","kind":"focus","atUnixMs":{}}},
            {{"student":"{student}","path":"/one.py","exercise":"one","kind":"edit","edits":9,"atUnixMs":{}}}]"#,
        now - 60_000,
        now - 30_000,
        now - 31_000,
    );
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-events")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", course.enrollment_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = get_with_cookie(&app, &format!("/api/overview?course=lt{s}"), &cookie).await;
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let groups = v["exercises"].as_array().unwrap();

    let holding = groups
        .iter()
        .find(|g| {
            g["students"]
                .as_array()
                .unwrap()
                .iter()
                .any(|st| st["student"] == student)
        })
        .expect("student appears somewhere");
    assert_eq!(
        holding["exercise"], "two",
        "the student stays on the exercise they moved to"
    );

    let st = holding["students"]
        .as_array()
        .unwrap()
        .iter()
        .find(|st| st["student"] == student)
        .unwrap();
    assert_eq!(
        st["editsRecent"], 0,
        "edits on the previous exercise don't count as typing here: {st}"
    );
}
