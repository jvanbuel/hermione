//! The live message stream: broadcasts to editors, and control frames to one.

use super::auth::bearer_token;
use super::auth::routing_student;
use super::auth::session_cookie;
use super::auth::verified_student;
use super::scope::resolve_course;
use crate::auth::AuthCtx;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::state::MessageOut;
use crate::student::Slot;
use crate::student::Student;
use crate::tenancy;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use futures::{SinkExt, StreamExt};
use hermione_entity::messages;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::Deserialize;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct WsQuery {
    /// Enrollment token, in the query. Deprecated: a URL is written to proxy and
    /// access logs, so current editors send it as a Bearer header instead. Still
    /// read so an editor that hasn't updated keeps working.
    token: Option<String>,
    /// Course slug (teacher/dashboard clients, paired with the session cookie).
    course: Option<String>,
    /// Resume after this message id (catch up on anything missed while away).
    /// Omit for live-only delivery (e.g. the dashboard monitor).
    since: Option<i64>,
    /// Which student this editor belongs to, so control frames meant for them
    /// (a snapshot request) reach only their socket. Extension clients only;
    /// a socket without it simply receives no control frames.
    student: Option<Student>,
}

/// Live message channel. Authenticates the same way the rest of the API does —
/// an enrollment token (students; a Bearer header, or `?token=` from older
/// editors) or the session cookie + course (teachers) —
/// then upgrades and streams that course's messages.
pub(super) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let (course_id, student) = listener(&state, &q, &headers).await?;
    Ok(ws.on_upgrade(move |socket| message_socket(socket, state, course_id, q.since, student)))
}

/// Who is connecting, and to which course: an editor with an enrollment token
/// (routed by student), or a teacher with a session (routed by nobody).
async fn listener(
    state: &AppState,
    q: &WsQuery,
    headers: &HeaderMap,
) -> ApiResult<(Uuid, Option<Student>)> {
    // The header wins over the query, so a client sending both is judged by the
    // one that isn't in a log.
    if let Some(token) = bearer_token(headers).or(q.token.as_deref()) {
        let course = tenancy::course_by_token(&state.db, token)
            .await
            .ok_or_else(|| ApiError::unauthorized("invalid enrollment token"))?;
        // An editor's socket is routed by student, so who it says it is has to be
        // true where students are verified.
        let verified = verified_student(state, headers)?;
        let student = routing_student(verified, q.student.clone())
            .map_err(|e| ApiError::unauthorized(e.to_string()))?;
        return Ok((course.id, student));
    }

    let ctx = match state
        .auth
        .admin_for(session_cookie(headers).as_deref())
        .await
    {
        Some(admin_id) => AuthCtx::Admin(admin_id),
        None if state.open_dev.load(Ordering::Relaxed) => AuthCtx::OpenDev,
        None => return Err(ApiError::unauthorized("login required")),
    };
    // A teacher's socket takes the course's messages only, never anyone's
    // control frames, whatever `student` it passes.
    let course_id = resolve_course(state, ctx, q.course.clone()).await?;
    Ok((course_id, None))
}

/// Sends any missed messages (when `since` is given), then tails live ones
/// alongside any control frames addressed to this socket's student.
/// Inbound frames are ignored for now — the hook where student→teacher chat
/// will land.
async fn message_socket(
    socket: WebSocket,
    state: AppState,
    course_id: Uuid,
    since: Option<i64>,
    student: Option<Student>,
) {
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

    // Then tail live messages, plus control frames when this socket said who it
    // belongs to. A socket without a student (the dashboard) gets messages only.
    let mut rx = state.msg_hub.subscribe(&course_id).await;
    let ctrl_key = student.map(|student| Slot {
        course: course_id,
        student,
    });
    let mut ctrl_rx = match &ctrl_key {
        Some(key) => Some(state.ctrl_hub.subscribe(key).await),
        None => None,
    };
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
            ctrl = async {
                match ctrl_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    // Nothing to tail: park this branch forever rather than
                    // spinning the select on a channel that doesn't exist.
                    None => std::future::pending().await,
                }
            } => {
                match ctrl {
                    Ok(c) => {
                        if sender.send(Message::Text(to_text(&c))).await.is_err() {
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

fn to_text<T: serde::Serialize>(msg: &T) -> axum::extract::ws::Utf8Bytes {
    serde_json::to_string(msg).unwrap_or_default().into()
}
