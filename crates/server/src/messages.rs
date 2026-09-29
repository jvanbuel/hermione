//! Teacher → students broadcast messages.
//!
//! An admin posts a message scoped to a course; each student's extension polls
//! the course inbox (authenticated by the enrollment token) and surfaces new
//! messages as editor notifications.

use axum::{
    extract::{Extension, Query, State},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use hermione_entity::messages;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use serde::Deserialize;

use crate::auth::AuthCtx;
use crate::error::{ApiError, ApiResult};
use crate::http::{resolve_course, routing_student, CourseCtx, VerifiedStudent};
use crate::state::{AppState, MessageOut};
use crate::student::{Audience, Student};

/// Max messages returned in a single inbox poll.
const POLL_LIMIT: u64 = 50;
/// Most students one message may be addressed to. A whole class fits many times over.
const MAX_RECIPIENTS: usize = 500;

#[derive(Deserialize)]
pub struct BroadcastRequest {
    course: Option<String>,
    text: String,
    /// Send to just these students. Absent is the whole course; present but
    /// empty is refused, since "no one" is never what was meant.
    students: Option<Vec<Student>>,
}

/// Which stored messages a listener may be shown: the course's own, and those
/// addressed to their student. A listener with no student (the dashboard) only
/// ever gets the course's own.
pub fn addressed_to(listener: Option<&Student>) -> Condition {
    let everyone = Condition::all().add(messages::Column::Student.is_null());
    match listener {
        Some(s) => Condition::any()
            .add(everyone)
            .add(messages::Column::Student.eq(s.as_str())),
        None => everyone,
    }
}

/// Admin posts a message to a course, or to some of its students
/// (teacher-authenticated, membership-checked).
pub async fn broadcast(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<BroadcastRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("empty message"));
    }
    let audiences: Vec<Audience> = match body.students {
        None => vec![Audience::Everyone],
        Some(students) => {
            let mut unique: Vec<Student> = Vec::with_capacity(students.len());
            for s in students {
                if !unique.contains(&s) {
                    unique.push(s);
                }
            }
            if unique.is_empty() {
                return Err(ApiError::bad_request("no students to send to"));
            }
            if unique.len() > MAX_RECIPIENTS {
                return Err(ApiError::bad_request("too many students in one message"));
            }
            unique.into_iter().map(Audience::Student).collect()
        }
    };
    let course_id = resolve_course(&state, ctx, body.course).await?;

    // One row per audience, all or nothing: a message that reached half the
    // students it was meant for is worse than one that failed visibly.
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let mut sent = Vec::with_capacity(audiences.len());
    for audience in audiences {
        let model = messages::ActiveModel {
            course_id: Set(course_id),
            body: Set(text.to_string()),
            student: Set(match &audience {
                Audience::Everyone => None,
                Audience::Student(s) => Some(s.as_str().to_string()),
            }),
            created_at: Set(now.into()),
            ..Default::default()
        };
        let row = messages::Entity::insert(model)
            .exec_with_returning(&txn)
            .await?;
        sent.push(MessageOut {
            id: row.id,
            body: row.body,
            created_at_unix_ms: row.created_at.timestamp_millis(),
            audience,
        });
    }
    txn.commit().await?;

    let (first, count) = (sent[0].id, sent.len());
    // Push to whoever is connected now; each socket keeps only what is for it.
    for message in sent {
        state.msg_hub.publish(&course_id, message).await;
    }
    Ok(Json(serde_json::json!({ "id": first, "sent": count })))
}

#[derive(Deserialize)]
pub struct PollQuery {
    /// Return messages with id greater than this (the client's last-seen id).
    since: Option<i64>,
    /// Which student is asking, where nothing verifies it (see `routing_student`).
    student: Option<Student>,
}

/// Student inbox: messages for the course identified by the enrollment token,
/// and those addressed to the asking student.
pub async fn inbox(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Query(q): Query<PollQuery>,
) -> ApiResult<impl IntoResponse> {
    let listener =
        routing_student(verified, q.student).map_err(|e| ApiError::unauthorized(e.to_string()))?;
    let rows = messages::Entity::find()
        .filter(messages::Column::CourseId.eq(course_id))
        .filter(messages::Column::Id.gt(q.since.unwrap_or(0)))
        .filter(addressed_to(listener.as_ref()))
        .order_by_asc(messages::Column::Id)
        .limit(POLL_LIMIT)
        .all(&state.db)
        .await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|m| MessageOut::try_from(m).ok())
            .collect::<Vec<_>>(),
    ))
}
