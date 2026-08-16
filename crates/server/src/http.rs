//! Axum HTTP server: REST session list, an SSE live stream for browsers, and
//! the bundled web viewer.

use std::convert::Infallible;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Path, Query, Request, State,
    },
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Redirect, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{SinkExt, StreamExt};
use hermione_entity::{messages, sessions, terminal_events};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::auth::{constant_time_eq, AuthCtx};
use crate::state::{AppState, MessageOut};
use crate::tenancy::{self, DEFAULT_COURSE_ID};

/// Static frontend assets, embedded at build time from `static/` (see build.rs).
mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

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

/// Rows read per page when replaying session history.
const HISTORY_PAGE: u64 = 500;
/// Name of the teacher session cookie.
const SESSION_COOKIE: &str = "hermione_session";

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
        .route(
            "/api/courses",
            get(list_courses).post(create_course_for_teacher),
        )
        .route("/api/courses/{slug}", get(get_course).patch(patch_course))
        .route("/api/courses/{slug}/rotate-token", post(rotate_token))
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
        .route("/api/overview", get(crate::files::overview))
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
        .route("/api/inbox", get(crate::messages::inbox))
        .route("/api/assistant/status", get(crate::assistant::status))
        .route("/api/assistant/chat", post(crate::assistant::chat))
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

// --- static frontend assets (embedded; no CDN, works offline) --------------

/// Looks up an embedded asset by its path relative to `static/`.
fn asset(path: &str) -> Option<&'static [u8]> {
    assets::ASSETS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, data)| *data)
}

/// Content-Type for an embedded asset, by extension.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Serves an embedded asset, or 404 if there's no such file. Unknown paths
/// (including any `..` traversal) simply don't match an embedded key.
fn serve_asset(path: &str) -> Response {
    match asset(path) {
        Some(bytes) => ([(header::CONTENT_TYPE, content_type(path))], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn index() -> Response {
    serve_asset("index.html")
}

async fn analytics_page() -> Response {
    serve_asset("analytics.html")
}

async fn transcripts_page() -> Response {
    serve_asset("transcripts.html")
}

async fn tokens_css() -> Response {
    serve_asset("tokens.css")
}

async fn vendor(Path(path): Path<String>) -> Response {
    serve_asset(&format!("vendor/{path}"))
}

/// Brand assets (logo mark, favicon). Public so the login page can show the
/// mark before a teacher is authenticated.
async fn brand_asset(Path(path): Path<String>) -> Response {
    serve_asset(&format!("assets/{path}"))
}

// --- authentication --------------------------------------------------------

async fn login_page() -> Response {
    serve_asset("login.html")
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(body): Json<LoginRequest>) -> Response {
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

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie(&headers) {
        state.auth.logout(&token).await;
    }
    let cleared = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, cleared)], StatusCode::NO_CONTENT).into_response()
}

/// Gate for teacher-only routes. Injects an `AuthCtx`; redirects browsers to
/// /login and returns 401 to API clients when unauthenticated.
async fn require_teacher(
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
async fn require_super_admin(
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

/// Gate for agent ingest: resolves the course enrollment token (which tenant
/// the data belongs to) and injects it as a `CourseCtx`.
async fn require_ingest(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let course_id = match token {
        Some(token) => match tenancy::course_by_token(&state.db, token).await {
            Some(course) => course.id,
            None => return (StatusCode::UNAUTHORIZED, "invalid enrollment token").into_response(),
        },
        // No token: only allowed in open dev mode, into the default course.
        None if state.open_dev.load(Ordering::Relaxed) => DEFAULT_COURSE_ID,
        None => return (StatusCode::UNAUTHORIZED, "enrollment token required").into_response(),
    };

    // Verified student identity (Hermione identity token), enforced when OIDC is
    // configured. When enforced, the trusted student replaces any self-asserted one.
    let verified = request
        .headers()
        .get("x-hermione-identity")
        .and_then(|v| v.to_str().ok())
        .and_then(|t| state.identity.verify(t));
    if state.identity.enforced() && verified.is_none() {
        return (StatusCode::UNAUTHORIZED, "verified identity required").into_response();
    }

    request.extensions_mut().insert(CourseCtx(course_id));
    request.extensions_mut().insert(VerifiedStudent(verified));
    next.run(request).await
}

// --- student identity (OIDC / GitHub) --------------------------------------

#[derive(Deserialize)]
struct ExchangeRequest {
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
async fn auth_exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> Response {
    match state.identity.verify_idp(&req.provider, &req.token).await {
        Ok(student) => issue_identity(&state, student),
        Err(e) => (StatusCode::UNAUTHORIZED, e).into_response(),
    }
}

#[derive(Deserialize)]
struct DeviceStartRequest {
    provider: String,
}

/// Begins the device-authorization flow for a CLI client.
async fn auth_device_start(
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
struct DevicePollRequest {
    provider: String,
    device_code: String,
}

/// Polls a pending device-flow authorization; returns a token once approved.
async fn auth_device_poll(
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
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == SESSION_COOKIE).then(|| v.to_string())
    })
}

// --- message stream (WebSocket) --------------------------------------------

#[derive(Deserialize)]
struct WsQuery {
    /// Enrollment token (extension clients).
    token: Option<String>,
    /// Course slug (teacher/dashboard clients, paired with the session cookie).
    course: Option<String>,
    /// Resume after this message id (catch up on anything missed while away).
    /// Omit for live-only delivery (e.g. the dashboard monitor).
    since: Option<i64>,
}

/// Live message channel. Authenticates the same way the rest of the API does —
/// an enrollment token (students) or the session cookie + course (teachers) —
/// then upgrades and streams that course's messages.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
) -> Response {
    let course_id = if let Some(token) = q.token.as_deref() {
        match tenancy::course_by_token(&state.db, token).await {
            Some(course) => course.id,
            None => return (StatusCode::UNAUTHORIZED, "invalid enrollment token").into_response(),
        }
    } else {
        let ctx = match state
            .auth
            .admin_for(session_cookie(&headers).as_deref())
            .await
        {
            Some(admin_id) => AuthCtx::Admin(admin_id),
            None if state.open_dev.load(Ordering::Relaxed) => AuthCtx::OpenDev,
            None => return (StatusCode::UNAUTHORIZED, "login required").into_response(),
        };
        match resolve_course(&state, ctx, q.course.clone()).await {
            Ok(id) => id,
            Err(resp) => return resp,
        }
    };

    ws.on_upgrade(move |socket| message_socket(socket, state, course_id, q.since))
}

/// Sends any missed messages (when `since` is given), then tails live ones.
/// Inbound frames are ignored for now — the hook where student→teacher chat
/// will land.
async fn message_socket(socket: WebSocket, state: AppState, course_id: Uuid, since: Option<i64>) {
    let (mut sender, mut receiver) = socket.split();

    // Catch up from the durable store (Postgres is the source of truth). Skipped
    // for live-only clients that don't pass `since`.
    if let Some(since) = since {
        if let Ok(rows) = messages::Entity::find()
            .filter(messages::Column::CourseId.eq(course_id))
            .filter(messages::Column::Id.gt(since))
            .order_by_asc(messages::Column::Id)
            .limit(50)
            .all(&state.db)
            .await
        {
            for m in rows {
                let dto = MessageOut {
                    id: m.id,
                    body: m.body,
                    created_at_unix_ms: m.created_at.timestamp_millis(),
                };
                if sender.send(Message::Text(to_text(&dto))).await.is_err() {
                    return;
                }
            }
        }
    }

    // Then tail live messages.
    let mut rx = state.msg_hub.subscribe(course_id).await;
    loop {
        tokio::select! {
            inbound = receiver.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {} // ignored until chat lands
                }
            }
            msg = rx.recv() => {
                match msg {
                    Ok(m) => {
                        if sender.send(Message::Text(to_text(&m))).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

fn to_text(msg: &MessageOut) -> axum::extract::ws::Utf8Bytes {
    serde_json::to_string(msg).unwrap_or_default().into()
}

// --- course scoping --------------------------------------------------------

/// Resolves the selected course (by slug, defaulting to "default") and checks
/// the caller may access it. Returns the course id or an error response.
pub async fn resolve_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: Option<String>,
) -> Result<Uuid, Response> {
    let slug = slug.unwrap_or_else(|| "default".to_string());
    let Some(course) = tenancy::course_by_slug(&state.db, &slug).await else {
        return Err((StatusCode::NOT_FOUND, "no such course").into_response());
    };
    match ctx {
        AuthCtx::OpenDev => Ok(course.id),
        AuthCtx::Admin(admin_id) => {
            if tenancy::is_member(&state.db, admin_id, course.id).await {
                Ok(course.id)
            } else {
                Err((StatusCode::FORBIDDEN, "not a member of this course").into_response())
            }
        }
    }
}

/// Loads a session and checks the caller may access its course.
async fn authorized_session(
    state: &AppState,
    ctx: AuthCtx,
    session_id: Uuid,
) -> Result<sessions::Model, Response> {
    let Some(session) = sessions::Entity::find_by_id(session_id)
        .one(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?
    else {
        return Err((StatusCode::NOT_FOUND, "no such session").into_response());
    };
    let course_id = session.course_id.unwrap_or(DEFAULT_COURSE_ID);
    match ctx {
        AuthCtx::OpenDev => Ok(session),
        AuthCtx::Admin(admin_id) if tenancy::is_member(&state.db, admin_id, course_id).await => {
            Ok(session)
        }
        AuthCtx::Admin(_) => {
            Err((StatusCode::FORBIDDEN, "not a member of this course").into_response())
        }
    }
}

// --- courses + provisioning ------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CourseDto {
    slug: String,
    name: String,
    repo_url: Option<String>,
    archived: bool,
}

impl From<hermione_entity::courses::Model> for CourseDto {
    fn from(c: hermione_entity::courses::Model) -> Self {
        CourseDto {
            slug: c.slug,
            name: c.name,
            repo_url: c.repo_url,
            archived: c.archived_at.is_some(),
        }
    }
}

/// `?archived=1` (or `true`) lists archived courses instead of active ones.
#[derive(Deserialize)]
struct ListCoursesQuery {
    archived: Option<String>,
}

/// Courses the caller may see (all of them in open dev mode). Active by default;
/// `?archived=1` returns archived ones (for the restore UI).
async fn list_courses(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<ListCoursesQuery>,
) -> Response {
    let archived = matches!(q.archived.as_deref(), Some("1" | "true" | "yes"));
    let courses = match ctx {
        AuthCtx::OpenDev => tenancy::all_courses(&state.db, archived).await,
        AuthCtx::Admin(admin_id) => tenancy::courses_for_admin(&state.db, admin_id, archived).await,
    };
    match courses {
        Ok(rows) => {
            let dtos: Vec<CourseDto> = rows.into_iter().map(CourseDto::from).collect();
            Json(dtos).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Normalizes a course slug to a URL-safe form (`[a-z0-9-]`), collapsing runs of
/// other characters into single dashes. Returns `None` if nothing usable remains.
pub(crate) fn normalize_slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// The repository's short name — the last path segment of a git URL, minus any
/// `.git` suffix. Understands both `https://host/owner/name.git` and
/// `git@host:owner/name.git` forms. Used to derive a default slug/name.
fn repo_short_name(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let tail = trimmed.rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Turns a slug into a human-friendly title ("intro-python" → "Intro Python").
pub(crate) fn title_from_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCourseBody {
    slug: Option<String>,
    name: Option<String>,
    repo_url: Option<String>,
    /// Seed the course's exercises from the linked repo's folders (GitHub only).
    /// Defaults to true when a GitHub repo is linked.
    seed_exercises: Option<bool>,
}

/// The resolved (slug, name, repo_url) for a new course, or a client error
/// describing what's missing/invalid. A repo URL alone is enough — the slug and
/// name are derived from it — which is what "link a repo as a course" means.
fn resolve_new_course(body: &CreateCourseBody) -> Result<(String, String, Option<String>), String> {
    let repo_url = body
        .repo_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Slug precedence: an explicit slug, else the repo's short name, else the
    // course name — so any one of the three fields is enough to create a course.
    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(explicit) => normalize_slug(explicit)
            .ok_or_else(|| "slug must contain a letter or digit".to_string())?,
        None => repo_url
            .as_deref()
            .and_then(repo_short_name)
            .and_then(|n| normalize_slug(&n))
            .or_else(|| body.name.as_deref().and_then(normalize_slug))
            .ok_or_else(|| "a name, slug, or repo URL is required".to_string())?,
    };

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| title_from_slug(&slug));

    Ok((slug, name, repo_url))
}

/// POST /api/courses — a signed-in teacher creates a course (optionally linked to
/// a git repo) and is automatically granted membership, so it appears in their
/// switcher right away.
async fn create_course_for_teacher(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<CreateCourseBody>,
) -> Response {
    let (slug, name, repo_url) = match resolve_new_course(&body) {
        Ok(parts) => parts,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    if tenancy::course_by_slug(&state.db, &slug).await.is_some() {
        return (
            StatusCode::CONFLICT,
            format!("a course with slug '{slug}' already exists"),
        )
            .into_response();
    }

    let course = match tenancy::create_course(&state.db, &slug, &name, repo_url.as_deref()).await {
        Ok(course) => course,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    // The creating teacher becomes a member; open-dev callers aren't a specific
    // admin, so there's nobody to grant (they can already see every course).
    if let AuthCtx::Admin(admin_id) = ctx {
        if let Err(e) = tenancy::grant_membership(&state.db, admin_id, course.id).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    // Best-effort: seed exercises from the linked repo's folders (GitHub only).
    // Failures here never fail course creation — they're reported as a note.
    let mut exercises_seeded = 0usize;
    let mut seed_note: Option<String> = None;
    let want_seed = body.seed_exercises.unwrap_or(true);
    if want_seed {
        if let Some(repo) = course.repo_url.as_deref() {
            match crate::repo::parse_github(repo) {
                Some((owner, name)) => {
                    // Only send the token to allow-listed owners, so a teacher
                    // can't disclose an unrelated private repo's folders through it.
                    let allowed = crate::repo::owner_allowed(&owner, &state.github_allowed_owners);
                    let token = state.github_token.as_deref().filter(|_| allowed);
                    match crate::repo::discover_exercises(&owner, &name, token).await {
                        Ok(found) if !found.is_empty() => {
                            let items: Vec<(String, String, i32)> = found
                                .iter()
                                .enumerate()
                                .map(|(i, e)| (e.slug.clone(), e.title.clone(), i as i32))
                                .collect();
                            match crate::exercises::upsert(&state.db, course.id, &items).await {
                                Ok(()) => exercises_seeded = items.len(),
                                Err(e) => {
                                    seed_note = Some(format!("could not save exercises: {e}"))
                                }
                            }
                        }
                        Ok(_) => {}
                        // When a token exists but this owner isn't allow-listed, say
                        // so — otherwise the failure looks like a missing token.
                        Err(e) if state.github_token.is_some() && !allowed => {
                            seed_note = Some(format!(
                                "{e} — owner '{owner}' is not in HERMIONE_GITHUB_ALLOWED_OWNERS, \
                                 so the GitHub token was not used"
                            ))
                        }
                        Err(e) => seed_note = Some(e),
                    }
                }
                None => seed_note = Some("exercise seeding supports GitHub repos only".to_string()),
            }
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "slug": course.slug,
            "name": course.name,
            "repoUrl": course.repo_url,
            "enrollmentToken": course.enrollment_token,
            "exercisesSeeded": exercises_seeded,
            "seedNote": seed_note,
        })),
    )
        .into_response()
}

// --- course detail, update, membership -------------------------------------

/// Resolves a course by slug and checks the caller may access it, returning the
/// full model (unlike `resolve_course`, which returns just the id).
async fn authorized_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: &str,
) -> Result<hermione_entity::courses::Model, Response> {
    let Some(course) = tenancy::course_by_slug(&state.db, slug).await else {
        return Err((StatusCode::NOT_FOUND, "no such course").into_response());
    };
    match ctx {
        AuthCtx::OpenDev => Ok(course),
        AuthCtx::Admin(admin_id) if tenancy::is_member(&state.db, admin_id, course.id).await => {
            Ok(course)
        }
        AuthCtx::Admin(_) => {
            Err((StatusCode::FORBIDDEN, "not a member of this course").into_response())
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CourseDetailDto {
    slug: String,
    name: String,
    repo_url: Option<String>,
    enrollment_token: String,
    archived: bool,
    members: Vec<String>,
}

/// GET /api/courses/{slug} — full detail incl. the enrollment token and members,
/// for the course-settings panel.
async fn get_course(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let members = match tenancy::admins_for_course(&state.db, course.id).await {
        Ok(rows) => rows.into_iter().map(|a| a.username).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    Json(CourseDetailDto {
        slug: course.slug,
        name: course.name,
        repo_url: course.repo_url,
        enrollment_token: course.enrollment_token,
        archived: course.archived_at.is_some(),
        members,
    })
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCourseBody {
    name: Option<String>,
    /// Present ⇒ set the linked repo; `null`/empty ⇒ clear it.
    #[serde(default, deserialize_with = "double_option")]
    repo_url: Option<Option<String>>,
    archived: Option<bool>,
}

/// PATCH /api/courses/{slug} — rename, relink the repo, or (un)archive.
async fn patch_course(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
    Json(body): Json<UpdateCourseBody>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let repo_url = body
        .repo_url
        .as_ref()
        .map(|inner| inner.as_deref().map(str::trim).filter(|s| !s.is_empty()));

    if name.is_some() || repo_url.is_some() {
        if let Err(e) = tenancy::update_course(&state.db, course.id, name, repo_url).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    if let Some(archived) = body.archived {
        if let Err(e) = tenancy::set_course_archived(&state.db, course.id, archived).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /api/courses/{slug}/rotate-token — issue a fresh enrollment token.
async fn rotate_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match tenancy::rotate_enrollment_token(&state.db, course.id).await {
        Ok(token) => Json(serde_json::json!({ "enrollmentToken": token })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct AddMemberBody {
    username: String,
}

/// POST /api/courses/{slug}/members — grant another existing admin access.
async fn add_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
    Json(body): Json<AddMemberBody>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let username = body.username.trim();
    let Some(admin_id) = tenancy::admin_by_username(&state.db, username).await else {
        return (
            StatusCode::NOT_FOUND,
            format!("no admin account named '{username}'"),
        )
            .into_response();
    };
    match tenancy::grant_membership(&state.db, admin_id, course.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// DELETE /api/courses/{slug}/members/{username} — revoke access. Refuses to
/// remove the last member (which would orphan the course).
async fn remove_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((slug, username)): Path<(String, String)>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(admin_id) = tenancy::admin_by_username(&state.db, username.trim()).await else {
        return (StatusCode::NOT_FOUND, "no such admin").into_response();
    };
    // Atomic: locks the membership rows so concurrent removals can't both slip
    // past the last-member check and orphan the course.
    match tenancy::revoke_membership_checked(&state.db, admin_id, course.id).await {
        Ok(tenancy::RevokeOutcome::Removed) => StatusCode::NO_CONTENT.into_response(),
        Ok(tenancy::RevokeOutcome::NotAMember) => {
            (StatusCode::NOT_FOUND, "not a member of this course").into_response()
        }
        Ok(tenancy::RevokeOutcome::LastMember) => (
            StatusCode::CONFLICT,
            "cannot remove the last member of a course",
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// serde helper: distinguishes an absent field from an explicit `null`, so PATCH
/// can tell "leave the repo alone" from "clear the repo".
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}

#[derive(Deserialize)]
struct CreateAdminRequest {
    username: String,
    password: String,
}

async fn create_admin_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateAdminRequest>,
) -> Response {
    match tenancy::create_admin(&state.db, &body.username, &body.password).await {
        Ok(admin) => {
            // First admin created: lock down the dashboard.
            state.open_dev.store(false, Ordering::Relaxed);
            Json(serde_json::json!({ "id": admin.id, "username": admin.username })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCourseRequest {
    slug: String,
    name: String,
    #[serde(default)]
    repo_url: Option<String>,
}

async fn create_course_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateCourseRequest>,
) -> Response {
    let repo_url = body
        .repo_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match tenancy::create_course(&state.db, &body.slug, &body.name, repo_url).await {
        Ok(course) => Json(serde_json::json!({
            "slug": course.slug,
            "name": course.name,
            "repoUrl": course.repo_url,
            "enrollmentToken": course.enrollment_token,
        }))
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Deserialize)]
struct GrantMembershipRequest {
    username: String,
    #[serde(rename = "courseSlug")]
    course_slug: String,
}

async fn grant_membership_handler(
    State(state): State<AppState>,
    Json(body): Json<GrantMembershipRequest>,
) -> Response {
    let Some(admin_id) = tenancy::admin_by_username(&state.db, &body.username).await else {
        return (StatusCode::NOT_FOUND, "no such admin").into_response();
    };
    let Some(course) = tenancy::course_by_slug(&state.db, &body.course_slug).await else {
        return (StatusCode::NOT_FOUND, "no such course").into_response();
    };
    match tenancy::grant_membership(&state.db, admin_id, course.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct SessionDto {
    id: String,
    student: String,
    command: String,
    status: String,
    cols: i32,
    rows: i32,
    started_at_unix_ms: i64,
    ended_at_unix_ms: Option<i64>,
    exit_code: Option<i32>,
}

impl From<sessions::Model> for SessionDto {
    fn from(m: sessions::Model) -> Self {
        SessionDto {
            id: m.id.to_string(),
            student: m.student,
            command: m.command,
            status: m.status,
            cols: m.cols,
            rows: m.rows,
            started_at_unix_ms: m.started_at.timestamp_millis(),
            ended_at_unix_ms: m.ended_at.map(|t| t.timestamp_millis()),
            exit_code: m.exit_code,
        }
    }
}

async fn list_sessions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> Response {
    let course_id = match resolve_course(&state, ctx, q.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match sessions::Entity::find()
        .filter(sessions::Column::CourseId.eq(course_id))
        .order_by_desc(sessions::Column::StartedAt)
        .all(&state.db)
        .await
    {
        Ok(rows) => {
            let dtos: Vec<SessionDto> = rows.into_iter().map(SessionDto::from).collect();
            Json(dtos).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct StreamQuery {
    /// Replay stored history before tailing live (default: true).
    history: Option<bool>,
}

/// One SSE payload, mirroring a `TerminalChunk`.
#[derive(Serialize)]
struct ChunkDto {
    stream: String,
    offset_ms: i64,
    /// base64-encoded raw terminal bytes (verbatim, with ANSI escapes).
    data: String,
    /// ANSI-stripped plain text of the same bytes.
    text: String,
}

async fn stream_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> axum::response::Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };
    if let Err(resp) = authorized_session(&state, ctx, id).await {
        return resp;
    }

    let include_history = query.history.unwrap_or(true);
    let rx = state.hub.subscribe(id).await;
    let db = state.db.clone();

    let stream = async_stream::stream! {
        if include_history {
            // Replay history in pages to bound memory for long sessions.
            let mut pages = terminal_events::Entity::find()
                .filter(terminal_events::Column::SessionId.eq(id))
                .order_by_asc(terminal_events::Column::Seq)
                .paginate(&db, HISTORY_PAGE);
            while let Ok(Some(rows)) = pages.fetch_and_next().await {
                for row in rows {
                    yield Ok::<Event, Infallible>(sse_from_row(&row));
                }
            }
        }

        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    let dto = ChunkDto {
                        stream: if chunk.stream == hermione_proto::v1::StreamKind::Stdin as i32 {
                            "stdin".into()
                        } else {
                            "stdout".into()
                        },
                        offset_ms: chunk.offset_ms,
                        data: BASE64.encode(&chunk.data),
                        text: crate::text::plain(&chunk.data),
                    };
                    yield Ok(json_event(&dto));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_from_row(row: &terminal_events::Model) -> Event {
    let dto = ChunkDto {
        stream: row.stream.clone(),
        offset_ms: row.offset_ms,
        data: row.data.clone(),
        text: row.text.clone().unwrap_or_else(|| {
            crate::text::plain(&BASE64.decode(row.data.as_bytes()).unwrap_or_default())
        }),
    };
    json_event(&dto)
}

fn json_event(dto: &ChunkDto) -> Event {
    // serde_json::to_string only fails on non-string map keys, not here.
    Event::default().data(serde_json::to_string(dto).unwrap_or_default())
}

#[derive(Deserialize)]
struct TranscriptQuery {
    /// "stdout" (default), "stdin", or "all".
    stream: Option<String>,
}

/// Returns the ANSI-stripped plain-text transcript of a session, in order.
async fn transcript(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> axum::response::Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };
    if let Err(resp) = authorized_session(&state, ctx, id).await {
        return resp;
    }

    let mut find = terminal_events::Entity::find()
        .filter(terminal_events::Column::SessionId.eq(id))
        .order_by_asc(terminal_events::Column::Seq);

    match query.stream.as_deref() {
        Some("stdin") => find = find.filter(terminal_events::Column::Stream.eq("stdin")),
        Some("all") => {}
        // Default to stdout: what the student actually saw.
        _ => find = find.filter(terminal_events::Column::Stream.eq("stdout")),
    }

    // Build the transcript a page at a time so we never hold the whole session
    // of rows in memory at once.
    let mut body = String::new();
    let mut pages = find.paginate(&state.db, HISTORY_PAGE);
    loop {
        match pages.fetch_and_next().await {
            Ok(Some(rows)) => {
                for r in rows {
                    body.push_str(&r.text.unwrap_or_else(|| {
                        crate::text::plain(&BASE64.decode(r.data.as_bytes()).unwrap_or_default())
                    }));
                }
            }
            Ok(None) => break,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        body,
    )
        .into_response()
}
