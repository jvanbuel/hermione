//! Who is calling: the teacher's session, the super-admin token, an editor's
//! enrollment token and verified identity, and the student sign-in routes.

use super::assets::serve_asset;
use crate::auth::constant_time_eq;
use crate::auth::AuthCtx;
use crate::state::AppState;
use crate::student::BlankName;
use crate::student::Student;
use crate::tenancy;
use crate::tenancy::DEFAULT_COURSE_ID;
use axum::extract::Request;
use axum::extract::State;
use axum::http::header;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Redirect;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde::Serialize;
use std::sync::atomic::Ordering;

use super::{CourseCtx, VerifiedStudent};
use axum::response::IntoResponse;

/// Name of the teacher session cookie.
const SESSION_COOKIE: &str = "hermione_session";

pub(super) async fn login_page() -> Response {
    serve_asset("login.html")
}

#[derive(Deserialize)]
pub(super) struct LoginRequest {
    username: String,
    password: String,
}

pub(super) async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Response {
    match tenancy::verify_login(&state.db, &body.username, &body.password).await {
        Some(admin_id) => {
            let token = state.auth.create_session(admin_id).await;
            let cookie = format!(
                "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200"
            );
            ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
        }
        None => (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
    }
}

pub(super) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie(&headers) {
        state.auth.logout(&token).await;
    }
    let cleared = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, cleared)], StatusCode::NO_CONTENT).into_response()
}

/// Gate for teacher-only routes. Injects an `AuthCtx`; redirects browsers to
/// /login and returns 401 to API clients when unauthenticated.
pub(super) async fn require_teacher(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = session_cookie(request.headers());
    let ctx = if let Some(admin_id) = state.auth.admin_for(token.as_deref()).await {
        Some(AuthCtx::Admin(admin_id))
    } else if state.open_dev.load(Ordering::Relaxed) {
        Some(AuthCtx::OpenDev)
    } else {
        None
    };

    match ctx {
        Some(ctx) => {
            request.extensions_mut().insert(ctx);
            next.run(request).await
        }
        None if request.uri().path().starts_with("/api/") => {
            (StatusCode::UNAUTHORIZED, "login required").into_response()
        }
        None => Redirect::to("/login").into_response(),
    }
}

/// Gate for the provisioning API: requires the super-admin secret.
pub(super) async fn require_super_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.admin_token.as_deref() else {
        return (StatusCode::FORBIDDEN, "provisioning API disabled").into_response();
    };
    let ok = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);
    if ok {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "invalid or missing admin token").into_response()
    }
}

/// The enrollment token an editor sends as `Authorization: Bearer <token>`.
pub(super) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

/// Gate for agent ingest: resolves the course enrollment token (which tenant
/// the data belongs to) and injects it as a `CourseCtx`.
pub(super) async fn require_ingest(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = bearer_token(request.headers());

    let course_id = match token {
        Some(token) => match tenancy::course_by_token(&state.db, token).await {
            Some(course) => course.id,
            None => return (StatusCode::UNAUTHORIZED, "invalid enrollment token").into_response(),
        },
        // No token: only allowed in open dev mode, into the default course.
        None if state.open_dev.load(Ordering::Relaxed) => DEFAULT_COURSE_ID,
        None => return (StatusCode::UNAUTHORIZED, "enrollment token required").into_response(),
    };

    // Verified student identity, enforced when OIDC is configured. When enforced,
    // the trusted student replaces any self-asserted one.
    let verified = match verified_student(&state, request.headers()) {
        Ok(verified) => verified,
        Err(refusal) => return refusal,
    };

    request.extensions_mut().insert(CourseCtx(course_id));
    request.extensions_mut().insert(VerifiedStudent(verified));
    next.run(request).await
}

/// Who a request's identity token says the student is (`x-hermione-identity`).
///
/// `Ok(None)` means the deployment doesn't verify students and this request
/// carries no valid token, so whatever name it asserts is all there is. `Err` is
/// the refusal to send back when the deployment does enforce identity and the
/// request has none. Shared by everything an editor connects to, so the routes
/// can't disagree about what "verified" means.
pub(super) fn verified_student(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<String>, Response> {
    let verified = headers
        .get("x-hermione-identity")
        .and_then(|v| v.to_str().ok())
        .and_then(|token| state.identity.verify(token));
    if state.identity.enforced() && verified.is_none() {
        return Err((StatusCode::UNAUTHORIZED, "verified identity required").into_response());
    }
    Ok(verified)
}

/// Which student a socket's control frames are for.
///
/// A verified identity wins. The name a socket claims is used only where nothing
/// verifies students: elsewhere it would be a way to listen in on someone
/// else's frames, and those frames say when a teacher is looking at their file.
pub(super) fn routing_student(
    verified: Option<String>,
    claimed: Option<Student>,
) -> Result<Option<Student>, BlankName> {
    match verified {
        Some(name) => Student::try_from(name).map(Some),
        None => Ok(claimed),
    }
}

// --- student identity (OIDC / GitHub) --------------------------------------

#[derive(Deserialize)]
pub(super) struct ExchangeRequest {
    provider: String,
    token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityResponse {
    identity_token: String,
    student: String,
    expires_in: i64,
}

/// Exchanges a verified IdP credential for a Hermione identity token.
pub(super) async fn auth_exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> Response {
    match state.identity.verify_idp(&req.provider, &req.token).await {
        Ok(student) => issue_identity(&state, student),
        Err(e) => (StatusCode::UNAUTHORIZED, e).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct DeviceStartRequest {
    provider: String,
}

/// Begins the device-authorization flow for a CLI client.
pub(super) async fn auth_device_start(
    State(state): State<AppState>,
    Json(req): Json<DeviceStartRequest>,
) -> Response {
    match state.identity.device_start(&req.provider).await {
        Ok(start) => Json(start).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DevicePollRequest {
    provider: String,
    device_code: String,
}

/// Polls a pending device-flow authorization; returns a token once approved.
pub(super) async fn auth_device_poll(
    State(state): State<AppState>,
    Json(req): Json<DevicePollRequest>,
) -> Response {
    match state
        .identity
        .device_poll(&req.provider, &req.device_code)
        .await
    {
        Ok(crate::identity::DevicePoll::Pending) => {
            Json(serde_json::json!({ "status": "pending" })).into_response()
        }
        Ok(crate::identity::DevicePoll::Done(student)) => issue_identity(&state, student),
        Err(e) => (StatusCode::UNAUTHORIZED, e).into_response(),
    }
}

fn issue_identity(state: &AppState, student: String) -> Response {
    match state.identity.issue(&student) {
        Some(identity_token) => Json(IdentityResponse {
            identity_token,
            student,
            expires_in: crate::identity::TOKEN_TTL_SECS,
        })
        .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "identity not configured").into_response(),
    }
}

/// Extracts the session token from the Cookie header.
pub(super) fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == SESSION_COOKIE).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn student(name: &str) -> Student {
        Student::try_from(name.to_string()).unwrap()
    }

    #[test]
    fn a_verified_identity_beats_the_name_a_socket_claims() {
        let routed = routing_student(Some("github:bob".into()), Some(student("alice")));
        assert_eq!(routed, Ok(Some(student("github:bob"))));
    }

    #[test]
    fn the_claimed_name_is_used_only_where_nothing_verifies() {
        assert_eq!(
            routing_student(None, Some(student("alice"))),
            Ok(Some(student("alice")))
        );
        assert_eq!(routing_student(None, None), Ok(None));
    }

    #[test]
    fn an_unusable_verified_identity_is_an_error_not_a_fallback() {
        // Falling back to the claimed name would let an editor pick its own on
        // exactly the deployments that verify them.
        assert_eq!(
            routing_student(Some(" ".into()), Some(student("alice"))),
            Err(BlankName)
        );
    }
}
