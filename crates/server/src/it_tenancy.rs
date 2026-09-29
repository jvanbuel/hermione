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
    let github = crate::github_access::GitHubAccess::default();
    app_with(identity, crate::solutions::Solutions::github(github)).await
}

async fn app_with(
    identity: crate::identity::Identity,
    solutions: crate::solutions::Solutions,
) -> (AppState, Router) {
    ensure_schema();
    let state = AppState {
        db: Database::connect(test_url())
            .await
            .expect("connect HERMIONE_TEST_DATABASE_URL"),
        hub: Hub::default(),
        msg_hub: MsgHub::default(),
        ctrl_hub: crate::state::CtrlHub::default(),
        snapshots: crate::snapshots::SnapshotStore::default(),
        auth: Auth::new(),
        identity,
        admin_token: Some("admintok".to_string()),
        open_dev: Arc::new(AtomicBool::new(false)),
        assistant: crate::assistant::Assistant::new(None, None),
        assistant_default_model: "claude-opus-4-8".to_string(),
        github: crate::github_access::GitHubAccess::default(),
        solutions,
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
async fn a_duplicate_username_is_a_conflict_and_leaks_no_database_text() {
    use axum::response::IntoResponse;
    let (state, _app) = app().await;
    let name = format!("dup{}", rnd());
    tenancy::create_admin(&state.db, &name, "pw").await.unwrap();

    let Err(tenancy::CreateAdminError::Db(dup)) =
        tenancy::create_admin(&state.db, &name, "pw").await
    else {
        panic!("a second admin with the same username must be refused");
    };
    let resp = crate::error::ApiError::already_exists_or_internal(dup, "taken").into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_string(resp).await, "taken");
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
        .unwrap()
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
        .unwrap()
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
async fn reference_solution_settings_are_validated_stored_and_reported() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("sol{s}"), "Sol", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("own{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("own{s}"), "pw").await.unwrap();
    let uri = format!("/api/courses/sol{s}");
    let patch = |body: &'static str| {
        let (app, uri, cookie) = (app.clone(), uri.clone(), cookie.clone());
        async move { req_with_cookie(&app, "PATCH", &uri, &cookie, Some(body)).await }
    };
    let detail = || {
        let (app, uri, cookie) = (app.clone(), uri.clone(), cookie.clone());
        async move {
            let resp = get_with_cookie(&app, &uri, &cookie).await;
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };
    let listed = || {
        let (app, cookie) = (app.clone(), cookie.clone());
        let slug = format!("sol{s}");
        async move {
            let resp = get_with_cookie(&app, "/api/courses", &cookie).await;
            let all: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
            all.as_array()
                .unwrap()
                .iter()
                .find(|c| c["slug"] == slug.as_str())
                .cloned()
                .unwrap()
        }
    };

    // Nothing configured to begin with.
    assert_eq!(listed().await["hasSolutions"], false);

    // A branch and a folder are stored in their canonical form and reported.
    let resp = patch(r#"{"solutionsRef":"  solutions ","solutionsDir":"answers/"}"#).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let d = detail().await;
    assert_eq!(
        (d["solutionsRef"].as_str(), d["solutionsDir"].as_str()),
        (Some("solutions"), Some("answers"))
    );
    let c = listed().await;
    assert_eq!(c["hasSolutions"], true);
    assert!(
        c.get("solutionsRef").is_none(),
        "the list says only whether, not where"
    );

    // Anything that could steer a GitHub request is refused, with a reason,
    // and changes nothing.
    for bad in [
        r#"{"solutionsRef":"a b"}"#,
        r#"{"solutionsRef":"x/../y"}"#,
        r#"{"solutionsRef":"a?b=c"}"#,
        r#"{"solutionsDir":"../secrets"}"#,
        r#"{"solutionsDir":"/etc"}"#,
        r#"{"solutionsDir":"a\\b"}"#,
    ] {
        let resp = patch(bad).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
        assert!(!body_string(resp).await.is_empty(), "{bad} says why");
    }
    let d = detail().await;
    assert_eq!(
        (d["solutionsRef"].as_str(), d["solutionsDir"].as_str()),
        (Some("solutions"), Some("answers")),
        "refusals change nothing"
    );

    // Each half clears on its own; with both gone the course has no solutions.
    assert_eq!(
        patch(r#"{"solutionsRef":null}"#).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        listed().await["hasSolutions"],
        true,
        "the folder alone is enough"
    );
    assert_eq!(
        patch(r#"{"solutionsDir":""}"#).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(listed().await["hasSolutions"], false);

    // Not somebody else's to change.
    tenancy::create_admin(&state.db, &format!("out{s}"), "pw")
        .await
        .unwrap();
    let outsider = login(&app, &format!("out{s}"), "pw").await.unwrap();
    let resp = req_with_cookie(
        &app,
        "PATCH",
        &uri,
        &outsider,
        Some(r#"{"solutionsRef":"x"}"#),
    )
    .await;
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

/// A file snapshot is posted with the course's enrollment token and read back
/// by a teacher of that course — and by nobody else. Snapshots carry the text a
/// student has on screen, so the scoping matters more here than anywhere.
#[tokio::test]
async fn file_snapshots_are_scoped_to_the_course() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("fs{s}"), "Fs", None)
        .await
        .unwrap();
    let admin = tenancy::create_admin(&state.db, &format!("fs{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, admin.id, course.id)
        .await
        .unwrap();
    let cookie = login(&app, &format!("fs{s}"), "pw").await.unwrap();

    // A second course, with a teacher who is a member of only that one.
    let other = tenancy::create_course(&state.db, &format!("fo{s}"), "Fo", None)
        .await
        .unwrap();
    let outsider = tenancy::create_admin(&state.db, &format!("fo{s}"), "pw")
        .await
        .unwrap();
    tenancy::grant_membership(&state.db, outsider.id, other.id)
        .await
        .unwrap();
    let other_cookie = login(&app, &format!("fo{s}"), "pw").await.unwrap();

    let student = format!("sn{s}");
    let payload = format!(
        r#"{{"student":"{student}","state":"file","path":"/w/ex1/main.py",
             "relativePath":"ex1/main.py","language":"python",
             "cursor":{{"line":4,"column":9}},
             "content":"print('hi')\n","baseline":{{"kind":"untracked"}}}}"#
    );

    // Unauthenticated ingest is refused, exactly like file events.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-snapshots")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/file-snapshots")
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

    // The course's own teacher sees the buffer.
    let uri = format!("/api/students/file?course=fs{s}&student={student}");
    let resp = get_with_cookie(&app, &uri, &cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let snapshot = &v["latest"]["snapshot"];
    assert_eq!(snapshot["state"], "file");
    assert_eq!(snapshot["content"], "print('hi')\n");
    assert_eq!(
        snapshot["cursor"],
        serde_json::json!({"line": 4, "column": 9})
    );
    assert_eq!(snapshot["baseline"]["kind"], "untracked");
    assert!(
        v["latest"]["ageMs"].as_u64().is_some(),
        "age is the server's"
    );

    // Highlighting is added by the server, one span list per screen line.
    let hl = snapshot["highlight"].as_array().expect("highlighted");
    assert_eq!(hl.len(), 2, "one list per line, trailing newline included");
    let rebuilt: String = hl[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t[1].as_str().unwrap())
        .collect();
    assert_eq!(rebuilt, "print('hi')", "spans rebuild the line verbatim");

    // A teacher of another course cannot reach it, by slug or by student name.
    let resp = get_with_cookie(&app, &uri, &other_cookie).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let other_uri = format!("/api/students/file?course=fo{s}&student={student}");
    let resp = get_with_cookie(&app, &other_uri, &other_cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        v["latest"].is_null(),
        "a snapshot must not leak into another course: {v}"
    );

    // And signing out entirely gets nothing.
    let resp = app
        .clone()
        .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK);
}

/// The wire format is a sum type, so reports that describe an impossible state
/// are refused rather than stored: a blank student, a cursor on line zero, a
/// diff line with no sign.
#[tokio::test]
async fn impossible_snapshots_are_refused_at_the_door() {
    let (state, app) = app().await;
    let s = rnd();
    let course = tenancy::create_course(&state.db, &format!("im{s}"), "Im", None)
        .await
        .unwrap();

    let post = |body: serde_json::Value| {
        let app = app.clone();
        let token = course.enrollment_token.clone();
        async move {
            app.oneshot(
                Request::post("/api/file-snapshots")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    let file = |patch: serde_json::Value| {
        let mut v = serde_json::json!({
            "student": "alice", "state": "file",
            "path": "/w/a.py", "relativePath": "a.py", "language": "python",
            "content": "x\n", "baseline": {"kind": "untracked"},
        });
        v.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        v
    };

    assert_eq!(post(file(serde_json::json!({}))).await, StatusCode::OK);
    assert_eq!(
        post(serde_json::json!({"student": "alice", "state": "declined"})).await,
        StatusCode::OK
    );
    for (why, bad) in [
        ("blank student", file(serde_json::json!({"student": " "}))),
        (
            "cursor on line 0",
            file(serde_json::json!({"cursor": {"line": 0, "column": 1}})),
        ),
        ("unknown state", file(serde_json::json!({"state": "maybe"}))),
        (
            "unsigned diff line",
            file(serde_json::json!({"baseline": {"kind": "head", "hunks": [
                {"oldStart": 1, "newStart": 1, "lines": ["oops"]}
            ]}})),
        ),
    ] {
        assert!(post(bad).await.is_client_error(), "{why} should be refused");
    }
}

// --- who a socket may listen as ---------------------------------------------
//
// A snapshot request goes down the control channel of the student whose file a
// teacher has open, which is to say the channel itself says who is being
// watched. These run a real server and a real WebSocket client, because the
// property is about what a connection is allowed to receive.

mod control_socket {
    use super::*;
    use crate::state::Control;
    use crate::student::{Slot, Student};
    use futures::StreamExt;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    fn student(name: &str) -> Student {
        Student::try_from(name.to_string()).unwrap()
    }

    fn github() -> crate::identity::Provider {
        use crate::identity::{Provider, ProviderKind};
        Provider {
            name: "github".into(),
            kind: ProviderKind::Github,
            issuer: None,
            client_id: "client".into(),
            client_secret: None,
            scopes: None,
        }
    }

    /// Serves the app on an ephemeral port and returns its address.
    async fn serve(app: Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect(
        addr: std::net::SocketAddr,
        query: &str,
        headers: &[(&str, &str)],
    ) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
        let mut request = format!("ws://{addr}/ws?{query}")
            .into_client_request()
            .unwrap();
        for (name, value) in headers {
            request.headers_mut().insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(s, _)| s)
    }

    /// The upgrade completes before the server subscribes the socket, so wait
    /// for the subscription rather than racing it.
    async fn listening(state: &AppState, slot: &Slot) {
        for _ in 0..100 {
            if state.ctrl_hub.is_listening(slot).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("nothing ever listened on {slot:?}");
    }

    /// The next text frame, or `None` if nothing arrives soon.
    async fn next_frame(socket: &mut Socket) -> Option<String> {
        match tokio::time::timeout(Duration::from_millis(400), socket.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => Some(t.to_string()),
            _ => None,
        }
    }

    async fn course(state: &AppState) -> hermione_entity::courses::Model {
        tenancy::create_course(&state.db, &format!("ws{}", rnd()), "Ws", None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_enrollment_token_can_travel_in_a_header_instead_of_the_url() {
        let (state, app) = app().await;
        let course = course(&state).await;
        let addr = serve(app).await;
        let bearer = format!("Bearer {}", course.enrollment_token);

        // Header alone: no secret in the URL, and the socket is Alice's.
        let mut socket = connect(addr, "student=alice", &[("authorization", &bearer)])
            .await
            .expect("a Bearer token is enough");
        let alice = Slot {
            course: course.id,
            student: student("alice"),
        };
        listening(&state, &alice).await;
        socket.close(None).await.ok();

        // The query form still works for editors that haven't updated.
        let query = format!("token={}&student=alice", course.enrollment_token);
        connect(addr, &query, &[])
            .await
            .expect("?token= is still accepted");

        // A wrong header is refused even beside a right query: the header wins.
        let refused = connect(addr, &query, &[("authorization", "Bearer nope")]).await;
        assert!(
            refused.is_err(),
            "the header is judged, not the query beside it"
        );

        // And no token at all is not a teacher's socket by accident.
        assert!(connect(addr, "student=alice", &[]).await.is_err());
    }

    #[tokio::test]
    async fn a_verified_identity_decides_whose_frames_a_socket_gets() {
        let identity = crate::identity::Identity::for_test("secret", vec![github()]);
        let bob_token = identity.issue("github:bob").unwrap();
        let (state, app) = app_with_identity(identity).await;
        let course = course(&state).await;
        let addr = serve(app).await;

        // Bob's editor claims to be alice — and proves it is bob.
        let query = format!("token={}&student=alice", course.enrollment_token);
        let mut socket = connect(addr, &query, &[("x-hermione-identity", &bob_token)])
            .await
            .expect("a verified editor may connect");

        let bob = Slot {
            course: course.id,
            student: student("github:bob"),
        };
        let alice = Slot {
            course: course.id,
            student: student("alice"),
        };
        listening(&state, &bob).await;
        assert!(
            !state.ctrl_hub.is_listening(&alice).await,
            "the claimed name must not have been listened on"
        );

        // A teacher opening alice's file says nothing to this socket...
        state
            .ctrl_hub
            .publish(&alice, Control::SnapshotRequest)
            .await;
        assert_eq!(next_frame(&mut socket).await, None);
        // ...and one for bob, whose identity it proved, gets through.
        state.ctrl_hub.publish(&bob, Control::SnapshotRequest).await;
        assert_eq!(
            next_frame(&mut socket).await.as_deref(),
            Some(r#"{"kind":"snapshot-request"}"#)
        );
    }

    #[tokio::test]
    async fn where_students_are_verified_an_unverified_socket_is_refused() {
        let identity = crate::identity::Identity::for_test("secret", vec![github()]);
        let (state, app) = app_with_identity(identity).await;
        let course = course(&state).await;
        let addr = serve(app).await;

        let query = format!("token={}&student=alice", course.enrollment_token);
        let refused = connect(addr, &query, &[]).await;
        let Err(tokio_tungstenite::tungstenite::Error::Http(response)) = refused else {
            panic!("expected the upgrade to be refused");
        };
        assert_eq!(response.status(), 401);
        assert!(
            !state
                .ctrl_hub
                .is_listening(&Slot {
                    course: course.id,
                    student: student("alice")
                })
                .await
        );
    }

    #[tokio::test]
    async fn where_nothing_verifies_students_the_claimed_name_routes() {
        let (state, app) = app().await;
        let course = course(&state).await;
        let addr = serve(app).await;

        let query = format!("token={}&student=alice", course.enrollment_token);
        let mut socket = connect(addr, &query, &[]).await.unwrap();
        let alice = Slot {
            course: course.id,
            student: student("alice"),
        };
        let bob = Slot {
            course: course.id,
            student: student("bob"),
        };
        listening(&state, &alice).await;

        state.ctrl_hub.publish(&bob, Control::SnapshotRequest).await;
        assert_eq!(
            next_frame(&mut socket).await,
            None,
            "another student's frame"
        );
        state
            .ctrl_hub
            .publish(&alice, Control::SnapshotRequest)
            .await;
        assert!(next_frame(&mut socket).await.is_some());
    }

    #[tokio::test]
    async fn a_teachers_socket_never_gets_control_frames() {
        let (state, app) = app().await;
        let s = rnd();
        let course = tenancy::create_course(&state.db, &format!("wt{s}"), "Wt", None)
            .await
            .unwrap();
        let admin = tenancy::create_admin(&state.db, &format!("wt{s}"), "pw")
            .await
            .unwrap();
        tenancy::grant_membership(&state.db, admin.id, course.id)
            .await
            .unwrap();
        let cookie = login(&app, &format!("wt{s}"), "pw").await.unwrap();
        let addr = serve(app).await;

        // The dashboard passes a student too; it must not become a listener.
        let query = format!("course=wt{s}&student=alice");
        let mut socket = connect(addr, &query, &[("cookie", &cookie)]).await.unwrap();
        let alice = Slot {
            course: course.id,
            student: student("alice"),
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !state.ctrl_hub.is_listening(&alice).await,
            "a teacher's socket must not subscribe to a student's frames"
        );
        state
            .ctrl_hub
            .publish(&alice, Control::SnapshotRequest)
            .await;
        assert_eq!(next_frame(&mut socket).await, None);
    }
}

/// A teacher, a course, a student's editor and a fake GitHub, wired through the
/// real HTTP stack.
mod reference_solutions {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::solutions::{Fake, FetchError, Solutions};

    struct Class {
        app: Router,
        cookie: String,
        slug: String,
        token: String,
        student: String,
        fake: Arc<Fake>,
        state: AppState,
    }

    /// `repo` is the course's linked repo; `solutions` its (ref, folder).
    async fn class(fake: Fake, repo: Option<&str>, solutions: Option<(&str, &str)>) -> Class {
        let fake = Arc::new(fake);
        let (state, app) = app_with(
            crate::identity::Identity::disabled(),
            Solutions::new(fake.clone()),
        )
        .await;
        let s = rnd();
        let course = tenancy::create_course(&state.db, &format!("rs{s}"), "Rs", repo)
            .await
            .unwrap();
        if let Some((git_ref, dir)) = solutions {
            let patch = tenancy::CoursePatch {
                solutions_ref: Some(Some(git_ref.to_string()).filter(|r| !r.is_empty())),
                solutions_dir: Some(Some(dir.to_string()).filter(|d| !d.is_empty())),
                ..Default::default()
            };
            tenancy::update_course(&state.db, course.id, &patch)
                .await
                .unwrap();
        }
        let admin = tenancy::create_admin(&state.db, &format!("rs{s}"), "pw")
            .await
            .unwrap();
        tenancy::grant_membership(&state.db, admin.id, course.id)
            .await
            .unwrap();
        let cookie = login(&app, &format!("rs{s}"), "pw").await.unwrap();
        Class {
            app,
            cookie,
            slug: format!("rs{s}"),
            token: course.enrollment_token,
            student: format!("st{s}"),
            fake,
            state,
        }
    }

    impl Class {
        /// The student's editor posts a file.
        async fn posts(&self, relative_path: &str, content: &str, truncated: bool) {
            let payload = serde_json::json!({
                "student": self.student, "state": "file",
                "path": format!("/w/{relative_path}"), "relativePath": relative_path,
                "language": "c", "content": content, "truncated": truncated,
                "baseline": {"kind": "untracked"},
            });
            let resp = self
                .app
                .clone()
                .oneshot(
                    Request::post("/api/file-snapshots")
                        .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(payload.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        /// The teacher polls, optionally asking for the comparison.
        async fn polls(&self, compare: bool) -> serde_json::Value {
            let extra = if compare { "&compare=solution" } else { "" };
            let uri = format!(
                "/api/students/file?course={}&student={}{extra}",
                self.slug, self.student
            );
            let resp = get_with_cookie(&self.app, &uri, &self.cookie).await;
            assert_eq!(resp.status(), StatusCode::OK);
            serde_json::from_str(&body_string(resp).await).unwrap()
        }

        fn fetches(&self) -> usize {
            self.fake.calls.load(Ordering::SeqCst)
        }
    }

    const REPO: Option<&str> = Some("https://github.com/acme/cs101");

    #[tokio::test]
    async fn a_file_that_matches_the_solution_says_so() {
        let fake = Fake::default().with("answers/ex1/a.c", Ok(Some("int main() {}\r\n".into())));
        let c = class(fake, REPO, Some(("solutions", "answers"))).await;
        c.posts("ex1/a.c", "int main() {}\n", false).await;
        let v = c.polls(true).await;
        assert_eq!(v["solution"]["state"], "identical");
        assert_eq!(
            v["solution"]["file"],
            serde_json::json!({"path": "answers/ex1/a.c", "ref": "solutions"}),
            "the reader sees which file it was checked against"
        );
    }

    #[tokio::test]
    async fn a_file_that_differs_comes_back_as_rows_with_the_solution_as_the_old_side() {
        let fake = Fake::default().with("ex1/a.c", Ok(Some("a\nb\nc\n".into())));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("ex1/a.c", "a\nB\nc\n", false).await;
        let s = c.polls(true).await["solution"].clone();
        assert_eq!(s["state"], "differs");
        assert_eq!(
            (s["added"].clone(), s["removed"].clone()),
            (1.into(), 1.into())
        );
        let rows = &s["hunks"][0]["rows"];
        assert_eq!(
            rows[1]["sign"], "-",
            "what the solution has and the student lacks"
        );
        assert_eq!(rows[1]["spans"][0][1], "b");
        assert_eq!(rows[2]["sign"], "+");
        assert_eq!(rows[2]["spans"][0][1], "B");
    }

    #[tokio::test]
    async fn nothing_is_read_from_github_unless_the_teacher_asks() {
        let fake = Fake::default().with("ex1/a.c", Ok(Some("x\n".into())));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("ex1/a.c", "x\n", false).await;
        let v = c.polls(false).await;
        assert!(v.get("solution").is_none());
        assert_eq!(c.fetches(), 0, "an ordinary poll costs GitHub nothing");
        assert_eq!(
            v["latest"]["snapshot"]["state"], "file",
            "the file itself still arrives"
        );
    }

    #[tokio::test]
    async fn repeated_polls_read_the_solution_once() {
        let fake = Fake::default().with("ex1/a.c", Ok(Some("x\n".into())));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("ex1/a.c", "x\n", false).await;
        for _ in 0..5 {
            c.polls(true).await;
        }
        assert_eq!(c.fetches(), 1);
    }

    #[tokio::test]
    async fn every_way_of_having_nothing_to_compare_says_which() {
        // No solutions configured.
        let c = class(Fake::default(), REPO, None).await;
        c.posts("a.c", "x\n", false).await;
        assert_eq!(c.polls(true).await["solution"]["state"], "unconfigured");

        // Configured, but no repository to read from.
        let c = class(Fake::default(), None, Some(("solutions", ""))).await;
        c.posts("a.c", "x\n", false).await;
        let s = c.polls(true).await["solution"].clone();
        assert_eq!(
            (s["state"].as_str(), s["code"].as_str()),
            (Some("unavailable"), Some("noRepository"))
        );
        assert!(s["message"]
            .as_str()
            .unwrap()
            .contains("no linked repository"));

        // A repository that isn't on GitHub.
        let c = class(
            Fake::default(),
            Some("https://gitlab.com/acme/cs101"),
            Some(("solutions", "")),
        )
        .await;
        c.posts("a.c", "x\n", false).await;
        assert_eq!(c.polls(true).await["solution"]["code"], "notGithub");
        assert_eq!(c.fetches(), 0, "neither of those reached for GitHub");

        // The solutions have no file for this one.
        let c = class(Fake::default(), REPO, Some(("solutions", ""))).await;
        c.posts("nope.c", "x\n", false).await;
        let s = c.polls(true).await["solution"].clone();
        assert_eq!(s["state"], "noReference");
        assert_eq!(s["file"]["path"], "nope.c");

        // GitHub said no.
        let fake = Fake::default().with("a.c", Err(FetchError::Denied));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("a.c", "x\n", false).await;
        let s = c.polls(true).await["solution"].clone();
        assert_eq!(
            (s["state"].as_str(), s["code"].as_str()),
            (Some("unavailable"), Some("denied"))
        );
    }

    #[tokio::test]
    async fn a_buffer_that_was_cut_short_is_not_compared() {
        let fake = Fake::default().with("a.c", Ok(Some("line 1\nline 2\n".into())));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("a.c", "line 1\n", true).await;
        assert_eq!(
            c.polls(true).await["solution"]["state"],
            "uncomparable",
            "else line 2 would show as missing from the student"
        );
    }

    #[tokio::test]
    async fn a_path_that_is_not_inside_the_repo_is_never_looked_up() {
        let c = class(Fake::default(), REPO, Some(("solutions", "answers"))).await;
        for hostile in [
            "/etc/passwd",
            "../../other/secret.c",
            "a/../../b.c",
            "C:\\Users\\x\\a.c",
        ] {
            c.posts(hostile, "x\n", false).await;
            assert_eq!(
                c.polls(true).await["solution"]["state"],
                "noReference",
                "{hostile}"
            );
        }
        assert_eq!(c.fetches(), 0, "none of them became a request");
    }

    #[tokio::test]
    async fn only_a_file_can_be_compared() {
        let c = class(Fake::default(), REPO, Some(("solutions", ""))).await;
        let v = c.polls(true).await;
        assert!(v.get("solution").is_none(), "nothing has arrived yet");

        let payload = serde_json::json!({"student": c.student, "state": "declined"});
        c.app
            .clone()
            .oneshot(
                Request::post("/api/file-snapshots")
                    .header(header::AUTHORIZATION, format!("Bearer {}", c.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            c.polls(true).await.get("solution").is_none(),
            "a refusal has nothing to compare"
        );
    }

    #[tokio::test]
    async fn only_the_courses_teachers_can_ask_and_students_never_get_the_solution() {
        let fake = Fake::default().with("a.c", Ok(Some("SECRET ANSWER\n".into())));
        let c = class(fake, REPO, Some(("solutions", ""))).await;
        c.posts("a.c", "x\n", false).await;
        let uri = format!(
            "/api/students/file?course={}&student={}&compare=solution",
            c.slug, c.student
        );

        // The student's own credential (the enrollment token) opens nothing here.
        let resp = c
            .app
            .clone()
            .oneshot(
                Request::get(&uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", c.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Nor does another course's teacher.
        let s = rnd();
        let other = tenancy::create_course(&c.state.db, &format!("ro{s}"), "Ro", None)
            .await
            .unwrap();
        let outsider = tenancy::create_admin(&c.state.db, &format!("ro{s}"), "pw")
            .await
            .unwrap();
        tenancy::grant_membership(&c.state.db, outsider.id, other.id)
            .await
            .unwrap();
        let theirs = login(&c.app, &format!("ro{s}"), "pw").await.unwrap();
        let resp = get_with_cookie(&c.app, &uri, &theirs).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // The ingest response — all a student's editor ever sees — carries nothing.
        let resp = c
            .app
            .clone()
            .oneshot(
                Request::post("/api/file-snapshots")
                    .header(header::AUTHORIZATION, format!("Bearer {}", c.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"student": c.student, "state": "empty"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!body_string(resp).await.contains("SECRET"));
        assert_eq!(c.fetches(), 0, "and none of that read the solution");
    }

    #[tokio::test]
    async fn an_unknown_comparison_is_refused() {
        let c = class(Fake::default(), REPO, Some(("solutions", ""))).await;
        let uri = format!(
            "/api/students/file?course={}&student={}&compare=everything",
            c.slug, c.student
        );
        let resp = get_with_cookie(&c.app, &uri, &c.cookie).await;
        assert!(resp.status().is_client_error(), "{}", resp.status());
    }
}
