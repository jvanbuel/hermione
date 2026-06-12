//! File-activity ingest and analytics for the VSCode extension.
//!
//! The extension POSTs lightweight JSON events (focus changes + periodic
//! heartbeats). We persist them and expose: the latest activity per student
//! (for live intervention) and time-on-task aggregates (for offline analysis).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use hermione_entity::file_events;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Gaps longer than this (seconds) between consecutive events are treated as
/// the student being away, and not counted as time-on-task.
const IDLE_GAP_SECS: i64 = 120;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEventIn {
    pub student: String,
    pub workspace: Option<String>,
    pub path: String,
    pub relative_path: Option<String>,
    pub language: Option<String>,
    pub exercise: Option<String>,
    pub kind: String,
    pub at_unix_ms: i64,
}

/// Accepts a batch of file events from the extension.
pub async fn ingest(
    State(state): State<AppState>,
    Json(events): Json<Vec<FileEventIn>>,
) -> impl IntoResponse {
    if events.is_empty() {
        return (StatusCode::OK, "0").into_response();
    }

    let models: Vec<file_events::ActiveModel> = events
        .into_iter()
        .map(|e| file_events::ActiveModel {
            student: Set(e.student),
            workspace: Set(e.workspace),
            path: Set(e.path),
            relative_path: Set(e.relative_path),
            language: Set(e.language),
            exercise: Set(e.exercise),
            kind: Set(e.kind),
            at: Set(unix_ms_to_dt(e.at_unix_ms)),
            created_at: Set(Utc::now().into()),
            ..Default::default()
        })
        .collect();

    let count = models.len();
    match file_events::Entity::insert_many(models)
        .exec(&state.db)
        .await
    {
        Ok(_) => (StatusCode::OK, count.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityDto {
    student: String,
    workspace: Option<String>,
    path: String,
    relative_path: Option<String>,
    language: Option<String>,
    exercise: Option<String>,
    kind: String,
    at_unix_ms: i64,
}

/// Latest file activity per student — what each student has open right now.
pub async fn students_activity(State(state): State<AppState>) -> impl IntoResponse {
    let rows = match file_events::Entity::find()
        .order_by_desc(file_events::Column::At)
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Keep the most recent event per student (rows are newest-first).
    let mut seen = std::collections::HashSet::new();
    let latest: Vec<ActivityDto> = rows
        .into_iter()
        .filter(|r| seen.insert(r.student.clone()))
        .map(|r| ActivityDto {
            student: r.student,
            workspace: r.workspace,
            path: r.path,
            relative_path: r.relative_path,
            language: r.language,
            exercise: r.exercise,
            kind: r.kind,
            at_unix_ms: r.at.timestamp_millis(),
        })
        .collect();

    Json(latest).into_response()
}

#[derive(Deserialize)]
pub struct AnalyticsQuery {
    student: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileTime {
    path: String,
    relative_path: Option<String>,
    exercise: Option<String>,
    seconds: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExerciseTime {
    exercise: String,
    seconds: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TimeReport {
    student: String,
    total_seconds: i64,
    per_file: Vec<FileTime>,
    per_exercise: Vec<ExerciseTime>,
}

/// Estimates time-on-task per file and per exercise for one student.
///
/// Attributes the gap between consecutive events to the earlier event's file,
/// capping each gap at `IDLE_GAP_SECS` so idle time isn't over-counted.
pub async fn time_per_file(
    State(state): State<AppState>,
    Query(q): Query<AnalyticsQuery>,
) -> impl IntoResponse {
    let rows = match file_events::Entity::find()
        .filter(file_events::Column::Student.eq(&q.student))
        .order_by_asc(file_events::Column::At)
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    use std::collections::HashMap;
    let mut per_file_secs: HashMap<String, (Option<String>, Option<String>, i64)> = HashMap::new();
    let mut per_exercise_secs: HashMap<String, i64> = HashMap::new();
    let mut total: i64 = 0;

    for pair in rows.windows(2) {
        let (cur, next) = (&pair[0], &pair[1]);
        if cur.kind == "close" {
            continue;
        }
        let gap = (next.at.timestamp() - cur.at.timestamp()).clamp(0, IDLE_GAP_SECS);
        if gap == 0 {
            continue;
        }
        total += gap;

        let entry = per_file_secs
            .entry(cur.path.clone())
            .or_insert((cur.relative_path.clone(), cur.exercise.clone(), 0));
        entry.2 += gap;

        if let Some(ex) = &cur.exercise {
            *per_exercise_secs.entry(ex.clone()).or_insert(0) += gap;
        }
    }

    let mut per_file: Vec<FileTime> = per_file_secs
        .into_iter()
        .map(|(path, (relative_path, exercise, seconds))| FileTime {
            path,
            relative_path,
            exercise,
            seconds,
        })
        .collect();
    per_file.sort_by(|a, b| b.seconds.cmp(&a.seconds));

    let mut per_exercise: Vec<ExerciseTime> = per_exercise_secs
        .into_iter()
        .map(|(exercise, seconds)| ExerciseTime { exercise, seconds })
        .collect();
    per_exercise.sort_by(|a, b| b.seconds.cmp(&a.seconds));

    Json(TimeReport {
        student: q.student,
        total_seconds: total,
        per_file,
        per_exercise,
    })
    .into_response()
}

fn unix_ms_to_dt(ms: i64) -> sea_orm::prelude::DateTimeWithTimeZone {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(Utc::now)
        .into()
}
