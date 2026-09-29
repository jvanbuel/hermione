//! The HTTP server: the routes, and the request types they share.
//!
//! One module per concern, so a change to (say) course settings doesn't sit in
//! a file with the terminal stream:
//!
//! - [`assets`] — the embedded pages, styles and scripts
//! - [`auth`] — teacher sessions, enrollment tokens, verified identity, sign-in
//! - [`scope`] — which course a request is about, and whether the caller may see it
//! - [`courses`] — listing, creating and editing courses and their teachers
//! - [`admin`] — provisioning with the super-admin token
//! - [`sessions`] — recorded terminal sessions: list, stream, transcript
//! - [`ws`] — the live message stream to editors

use axum::{
    middleware,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::state::AppState;

mod admin;
mod assets;
mod auth;
mod courses;
mod scope;
mod sessions;
mod ws;

use admin::{create_admin_handler, create_course_handler, grant_membership_handler};
use assets::{
    analytics_page, brand_asset, index, recap_page, tokens_css, transcripts_page, vendor,
};
use auth::{
    auth_device_poll, auth_device_start, auth_exchange, login, login_page, logout, require_ingest,
    require_super_admin, require_teacher,
};
use courses::{
    add_member, course_tree, create_course_for_teacher, get_course, list_courses, patch_course,
    remove_member, rotate_token,
};
use sessions::{list_sessions, stream_session, transcript};
use ws::ws_handler;

// What the rest of the crate reaches for by `crate::http::…`.
pub(crate) use auth::routing_student;
pub(crate) use courses::{normalize_slug, title_from_slug};
pub(crate) use scope::{authorized_course, resolve_course};

/// A request's resolved course (tenant), injected by ingest auth.
#[derive(Clone, Copy)]
pub struct CourseCtx(pub Uuid);

/// The verified student id for an ingest request (from a Hermione identity
/// token). `None` when identity verification is not enforced.
#[derive(Clone)]
pub struct VerifiedStudent(pub Option<String>);

/// Common `?course=<slug>` selector for dashboard endpoints.
#[derive(Deserialize)]
pub struct CourseQuery {
    pub course: Option<String>,
}

pub fn router(state: AppState) -> Router {
    // Public routes: login + vendored static assets (not sensitive).
    let public = Router::new()
        .route("/login", get(login_page))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/healthz", get(|| async { "ok" }))
        // Student identity: authenticate with an IdP, get a Hermione token.
        .route("/api/auth/exchange", post(auth_exchange))
        .route("/api/auth/device/start", post(auth_device_start))
        .route("/api/auth/device/poll", post(auth_device_poll))
        // The message stream authenticates itself (enrollment token or cookie).
        .route("/ws", get(ws_handler))
        .route("/tokens.css", get(tokens_css))
        .route("/assets/{*path}", get(brand_asset))
        .route("/vendor/{*path}", get(vendor));

    // Teacher-only routes: the dashboard and everything that exposes student
    // data, all scoped to a course the caller may access.
    let protected = Router::new()
        .route("/", get(index))
        .route("/analytics", get(analytics_page))
        .route("/transcripts", get(transcripts_page))
        .route("/recap", get(recap_page))
        .route(
            "/api/courses",
            get(list_courses).post(create_course_for_teacher),
        )
        .route("/api/courses/{slug}", get(get_course).patch(patch_course))
        .route("/api/courses/{slug}/rotate-token", post(rotate_token))
        .route("/api/courses/{slug}/tree", get(course_tree))
        .route("/api/courses/{slug}/members", post(add_member))
        .route(
            "/api/courses/{slug}/members/{username}",
            delete(remove_member),
        )
        .route(
            "/api/assistant/conversations",
            get(crate::assistant::list_conversations),
        )
        .route(
            "/api/assistant/conversations/{id}/messages",
            get(crate::assistant::conversation_messages),
        )
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}/stream", get(stream_session))
        .route("/api/sessions/{id}/transcript", get(transcript))
        .route(
            "/api/students/activity",
            get(crate::files::students_activity),
        )
        .route("/api/students/file", get(crate::snapshots::student_file))
        .route("/api/overview", get(crate::files::overview))
        .route("/api/recap", get(crate::recap::handler))
        .route(
            "/api/analytics/time-per-file",
            get(crate::files::time_per_file),
        )
        .route(
            "/api/exercises",
            get(crate::exercises::list).post(crate::exercises::define),
        )
        .route(
            "/api/assistant",
            get(crate::assistant::get_config).put(crate::assistant::put_config),
        )
        .route("/api/messages", post(crate::messages::broadcast))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_teacher,
        ));

    // Provisioning API, guarded by the super-admin secret.
    let admin = Router::new()
        .route("/api/admin/admins", post(create_admin_handler))
        .route("/api/admin/courses", post(create_course_handler))
        .route("/api/admin/memberships", post(grant_membership_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_super_admin,
        ));

    // Agent ingest: authenticated by a course enrollment token, which also
    // determines the tenant the data lands in. The student inbox is read with
    // the same credential.
    let ingest = Router::new()
        .route("/api/file-events", post(crate::files::ingest))
        .route("/api/file-snapshots", post(crate::snapshots::ingest))
        .route("/api/inbox", get(crate::messages::inbox))
        .route("/api/assistant/status", get(crate::assistant::status))
        .route(
            "/api/assistant/chat/stream",
            post(crate::assistant::chat_stream),
        )
        .route("/api/assistant/history", get(crate::assistant::history))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_ingest,
        ));

    public
        .merge(protected)
        .merge(admin)
        .merge(ingest)
        .layer(CorsLayer::permissive())
        .with_state(state)
}
