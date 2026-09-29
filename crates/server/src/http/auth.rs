//! Who is calling: the teacher's session, the super-admin token, an editor's
//! enrollment token and verified identity, and the student sign-in routes.

use std::sync::atomic::Ordering;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::assets::serve_asset;
use super::{CourseCtx, VerifiedStudent};
use crate::auth::{constant_time_eq, AuthCtx};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::student::{BlankName, Student};
use crate::tenancy::{self, DEFAULT_COURSE_ID};

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
) -> ApiResult {
    let admin_id = tenancy::verify_login(&state.db, &body.username, &body.password)
        .await
        .ok_or_else(|| ApiError::unauthorized("invalid credentials"))?;
    let token = state.auth.create_session(admin_id).await;
    let cookie =
        format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200");
    Ok(([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response())
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
) -> ApiResult {
    let expected = state
        .admin_token
        .as_deref()
        .ok_or_else(|| ApiError::forbidden("provisioning API disabled"))?;
    let presented = bearer_token(request.headers())
        .is_some_and(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()));
    if !presented {
        return Err(ApiError::unauthorized("invalid or missing admin token"));
    }
    Ok(next.run(request).await)
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
) -> ApiResult {
    let course_id = match bearer_token(request.headers()) {
        Some(token) => {
            tenancy::course_by_token(&state.db, token)
                .await
                .ok_or_else(|| ApiError::unauthorized("invalid enrollment token"))?
                .id
        }
        // No token: only allowed in open dev mode, into the default course.
        None if state.open_dev.load(Ordering::Relaxed) => DEFAULT_COURSE_ID,
        None => return Err(ApiError::unauthorized("enrollment token required")),
    };

    // Verified student identity, enforced when OIDC is configured. When enforced,
    // the trusted student replaces any self-asserted one.
    let verified = verified_student(&state, request.headers())?;

    request.extensions_mut().insert(CourseCtx(course_id));
    request.extensions_mut().insert(VerifiedStudent(verified));
    Ok(next.run(request).await)
}

/// Who a request's identity token says the student is (`x-hermione-identity`).
///
/// `Ok(None)` means the deployment doesn't verify students and this request
/// carries no valid token, so whatever name it asserts is all there is. `Err` is
/// the refusal when the deployment does enforce identity and the request has
/// none. Shared by everything an editor connects to, so the routes
/// can't disagree about what "verified" means.
pub(super) fn verified_student(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<String>> {
    let verified = headers
        .get("x-hermione-identity")
        .and_then(|v| v.to_str().ok())
        .and_then(|token| state.identity.verify(token));
    if state.identity.enforced() && verified.is_none() {
        return Err(ApiError::unauthorized("verified identity required"));
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
pub(super) struct IdentityResponse {
    identity_token: String,
    student: String,
    expires_in: i64,
}

/// Exchanges a verified IdP credential for a Hermione identity token.
pub(super) async fn auth_exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> ApiResult<Json<IdentityResponse>> {
    let student = state
        .identity
        .verify_idp(&req.provider, &req.token)
        .await
        .map_err(ApiError::unauthorized)?;
    issue_identity(&state, student)
}

#[derive(Deserialize)]
pub(super) struct DeviceStartRequest {
    provider: String,
}

/// Begins the device-authorization flow for a CLI client.
pub(super) async fn auth_device_start(
    State(state): State<AppState>,
    Json(req): Json<DeviceStartRequest>,
) -> ApiResult<Json<crate::identity::DeviceStart>> {
    let start = state
        .identity
        .device_start(&req.provider)
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(start))
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
) -> ApiResult {
    let polled = state
        .identity
        .device_poll(&req.provider, &req.device_code)
        .await
        .map_err(ApiError::unauthorized)?;
    match polled {
        crate::identity::DevicePoll::Pending => {
            Ok(Json(serde_json::json!({ "status": "pending" })).into_response())
        }
        crate::identity::DevicePoll::Done(student) => {
            Ok(issue_identity(&state, student)?.into_response())
        }
    }
}

fn issue_identity(state: &AppState, student: String) -> ApiResult<Json<IdentityResponse>> {
    let identity_token = state.identity.issue(&student).ok_or_else(|| {
        ApiError::refused(StatusCode::SERVICE_UNAVAILABLE, "identity not configured")
    })?;
    Ok(Json(IdentityResponse {
        identity_token,
        student,
        expires_in: crate::identity::TOKEN_TTL_SECS,
    }))
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
