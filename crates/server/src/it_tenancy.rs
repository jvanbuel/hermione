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
    ensure_schema();
    let state = AppState {
        db: Database::connect(test_url())
            .await
            .expect("connect HERMIONE_TEST_DATABASE_URL"),
        hub: Hub::default(),
        msg_hub: MsgHub::default(),
        auth: Auth::new(),
        admin_token: Some("admintok".to_string()),
        open_dev: Arc::new(AtomicBool::new(false)),
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
    let mine = tenancy::create_course(&state.db, &format!("mine{s}"), "Mine")
        .await
        .unwrap();
    tenancy::create_course(&state.db, &format!("other{s}"), "Other")
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
    let x = tenancy::create_course(&state.db, &format!("x{s}"), "X")
        .await
        .unwrap();
    let y = tenancy::create_course(&state.db, &format!("y{s}"), "Y")
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
