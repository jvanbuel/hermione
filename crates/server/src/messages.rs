//! Teacher → students broadcast messages.
//!
//! An admin posts a message scoped to a course; each student's extension polls
//! the course inbox (authenticated by the enrollment token) and surfaces new
//! messages as editor notifications.

use axum::{
    extract::{Extension, Query, State},
    Json,
};
use chrono::Utc;
use hermione_entity::messages;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use crate::auth::AuthCtx;
use crate::error::{ApiError, ApiResult};
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
) -> ApiResult<Json<serde_json::Value>> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("empty message"));
    }
    let course_id = resolve_course(&state, ctx, body.course).await?;

    let model = messages::ActiveModel {
        course_id: Set(course_id),
        body: Set(text.to_string()),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    let msg = messages::Entity::insert(model)
        .exec_with_returning(&state.db)
        .await?;
    // Push to everyone currently connected for this course.
    state
        .msg_hub
        .publish(
            &course_id,
            crate::state::MessageOut {
                id: msg.id,
                body: msg.body,
                created_at_unix_ms: msg.created_at.timestamp_millis(),
            },
        )
        .await;
    Ok(Json(serde_json::json!({ "id": msg.id })))
}

#[derive(Deserialize)]
pub struct PollQuery {
    /// Return messages with id greater than this (the client's last-seen id).
    since: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDto {
    id: i64,
    body: String,
    created_at_unix_ms: i64,
}

/// Student inbox: messages for the course identified by the enrollment token.
pub async fn inbox(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Query(q): Query<PollQuery>,
) -> ApiResult<Json<Vec<MessageDto>>> {
    let rows = messages::Entity::find()
        .filter(messages::Column::CourseId.eq(course_id))
        .filter(messages::Column::Id.gt(q.since.unwrap_or(0)))
        .order_by_asc(messages::Column::Id)
        .limit(POLL_LIMIT)
        .all(&state.db)
        .await?;
    Ok(Json(
        rows.into_iter()
            .map(|m| MessageDto {
                id: m.id,
                body: m.body,
                created_at_unix_ms: m.created_at.timestamp_millis(),
            })
            .collect(),
    ))
}
