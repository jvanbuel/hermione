//! Teacher → students broadcast messages.
//!
//! An admin posts a message scoped to a course; each student's extension polls
//! the course inbox (authenticated by the enrollment token) and surfaces new
//! messages as editor notifications.

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use hermione_entity::messages;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use crate::auth::AuthCtx;
use crate::http::{resolve_course, CourseCtx};
use crate::state::AppState;

/// Max messages returned in a single inbox poll.
const POLL_LIMIT: u64 = 50;

#[derive(Deserialize)]
pub struct BroadcastRequest {
    course: Option<String>,
    text: String,
}

/// Admin posts a broadcast to a course (teacher-authenticated, membership-checked).
pub async fn broadcast(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<BroadcastRequest>,
) -> Response {
    let text = body.text.trim();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty message").into_response();
    }
    let course_id = match resolve_course(&state, ctx, body.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let model = messages::ActiveModel {
        course_id: Set(course_id),
        body: Set(text.to_string()),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    match messages::Entity::insert(model)
        .exec_with_returning(&state.db)
        .await
    {
        Ok(msg) => Json(serde_json::json!({ "id": msg.id })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct PollQuery {
    /// Return messages with id greater than this (the client's last-seen id).
    since: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageDto {
    id: i64,
    body: String,
    created_at_unix_ms: i64,
}

/// Student inbox: messages for the course identified by the enrollment token.
pub async fn inbox(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Query(q): Query<PollQuery>,
) -> Response {
    let since = q.since.unwrap_or(0);
    match messages::Entity::find()
        .filter(messages::Column::CourseId.eq(course_id))
        .filter(messages::Column::Id.gt(since))
        .order_by_asc(messages::Column::Id)
        .limit(POLL_LIMIT)
        .all(&state.db)
        .await
    {
        Ok(rows) => {
            let dtos: Vec<MessageDto> = rows
                .into_iter()
                .map(|m| MessageDto {
                    id: m.id,
                    body: m.body,
                    created_at_unix_ms: m.created_at.timestamp_millis(),
                })
                .collect();
            Json(dtos).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
