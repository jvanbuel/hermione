//! Axum HTTP server: REST session list, an SSE live stream for browsers, and
//! the bundled web viewer.

use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hermione_entity::{sessions, terminal_events};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/vendor/xterm.js", get(xterm_js))
        .route("/vendor/xterm.css", get(xterm_css))
        .route("/vendor/addon-fit.js", get(addon_fit_js))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}/stream", get(stream_session))
        .route("/api/sessions/{id}/transcript", get(transcript))
        .route("/api/file-events", post(crate::files::ingest))
        .route("/api/students/activity", get(crate::files::students_activity))
        .route("/api/analytics/time-per-file", get(crate::files::time_per_file))
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
            if let Ok(rows) = terminal_events::Entity::find()
                .filter(terminal_events::Column::SessionId.eq(id))
                .order_by_asc(terminal_events::Column::Seq)
                .all(&db)
                .await
            {
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

    match find.all(&state.db).await {
        Ok(rows) => {
            let body: String = rows
                .iter()
                .map(|r| {
                    r.text.clone().unwrap_or_else(|| {
                        crate::text::plain(&BASE64.decode(r.data.as_bytes()).unwrap_or_default())
                    })
                })
                .collect();
            ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
