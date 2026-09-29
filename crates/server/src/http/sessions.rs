//! Recorded terminal sessions: listing, replaying and reading them.

use super::scope::authorized_session;
use super::scope::resolve_course;
use crate::auth::AuthCtx;
use crate::state::AppState;
use axum::extract::Extension;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::sse::KeepAlive;
use axum::response::sse::Sse;
use axum::response::Response;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use hermione_entity::sessions;
use hermione_entity::terminal_events;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::CourseQuery;
use axum::response::IntoResponse;
use base64::Engine;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};

/// Rows read per page when replaying session history.
const HISTORY_PAGE: u64 = 500;

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

pub(super) async fn list_sessions(
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
pub(super) struct StreamQuery {
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

pub(super) async fn stream_session(
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
    let rx = state.hub.subscribe(&id).await;
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
pub(super) struct TranscriptQuery {
    /// "stdout" (default), "stdin", or "all".
    stream: Option<String>,
}

/// Returns the ANSI-stripped plain-text transcript of a session, in order.
pub(super) async fn transcript(
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
