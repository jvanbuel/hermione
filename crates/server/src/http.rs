//! Axum HTTP server: REST session list, an SSE live stream for browsers, and
//! the bundled web viewer.

use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Json, Router,
};
use axum::extract::Request;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hermione_entity::{sessions, terminal_events};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::state::AppState;

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
        .route("/vendor/xterm.js", get(xterm_js))
        .route("/vendor/xterm.css", get(xterm_css))
        .route("/vendor/addon-fit.js", get(addon_fit_js));

    // Teacher-only routes: the dashboard and everything that exposes student data.
    let protected = Router::new()
        .route("/", get(index))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}/stream", get(stream_session))
        .route("/api/sessions/{id}/transcript", get(transcript))
        .route("/api/students/activity", get(crate::files::students_activity))
        .route("/api/overview", get(crate::files::overview))
        .route("/api/analytics/time-per-file", get(crate::files::time_per_file))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_teacher));

    // Agent ingest: authenticated with the shared bearer token instead.
    let ingest = Router::new()
        .route("/api/file-events", post(crate::files::ingest))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_ingest_token,
        ));

    public
        .merge(protected)
        .merge(ingest)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

// xterm.js assets are vendored (no external CDN) so the viewer works offline
// and in locked-down networks.
async fn xterm_js() -> impl IntoResponse {
    js(include_str!("../static/vendor/xterm.js"))
}

async fn addon_fit_js() -> impl IntoResponse {
    js(include_str!("../static/vendor/addon-fit.js"))
}

async fn xterm_css() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/vendor/xterm.css"),
    )
}

fn js(body: &'static str) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        body,
    )
}

// --- authentication --------------------------------------------------------

async fn login_page() -> Html<&'static str> {
    Html(include_str!("../static/login.html"))
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Response {
    match state.auth.login(&body.password).await {
        Some(token) => {
            let cookie = format!(
                "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200"
            );
            ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
        }
        None => (StatusCode::UNAUTHORIZED, "invalid password").into_response(),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie(&headers) {
        state.auth.logout(&token).await;
    }
    let cleared = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, cleared)], StatusCode::NO_CONTENT).into_response()
}

/// Gate for teacher-only routes. Redirects browsers to /login, returns 401 to
/// API clients.
async fn require_teacher(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let token = session_cookie(request.headers());
    if state.auth.validate(token.as_deref()).await {
        return next.run(request).await;
    }
    if request.uri().path().starts_with("/api/") {
        (StatusCode::UNAUTHORIZED, "login required").into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

/// Gate for agent ingest: requires the shared bearer token (when configured).
async fn require_ingest_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.ingest_token.as_deref() else {
        return next.run(request).await; // check disabled
    };
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if presented == Some(expected) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "invalid or missing token").into_response()
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

async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    match sessions::Entity::find()
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
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> axum::response::Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };

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
        text: row
            .text
            .clone()
            .unwrap_or_else(|| crate::text::plain(&BASE64.decode(row.data.as_bytes()).unwrap_or_default())),
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
    Path(id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> axum::response::Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };

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
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}
