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
use hermione_entity::{file_events, sessions};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Gaps longer than this (seconds) between consecutive events are treated as
/// the student being away, and not counted as time-on-task.
const IDLE_GAP_SECS: i64 = 120;

/// A student last seen within this many seconds is considered "active".
const ACTIVE_WINDOW_SECS: i64 = 90;

/// Live views (overview, activity) only consider events from the recent past —
/// roughly one teaching session — so their cost stays bounded as history grows.
const RECENT_WINDOW_HOURS: i64 = 8;

/// Timestamp marking the start of the "recent" window.
fn recent_cutoff() -> sea_orm::prelude::DateTimeWithTimeZone {
    (Utc::now() - chrono::Duration::hours(RECENT_WINDOW_HOURS)).into()
}

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
        .filter(file_events::Column::At.gt(recent_cutoff()))
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

// ---------------------------------------------------------------------------
// Overview: students grouped by the exercise they're currently working on,
// with time-on-task and a link to their terminal — everything the dashboard's
// main board needs, in one call.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OverviewStudent {
    student: String,
    file: Option<String>,
    language: Option<String>,
    exercise: Option<String>,
    last_seen_unix_ms: i64,
    /// "active" if seen recently, else "idle".
    status: String,
    /// Accumulated time-on-task for the current exercise (seconds).
    seconds_on_exercise: i64,
    /// When the student first touched the current exercise.
    started_exercise_unix_ms: Option<i64>,
    /// Latest terminal session for this student, if any.
    terminal_session_id: Option<String>,
    terminal_status: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExerciseGroup {
    exercise: String,
    students: Vec<OverviewStudent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    exercises: Vec<ExerciseGroup>,
    /// Students whose current file maps to no exercise.
    no_exercise: Vec<OverviewStudent>,
}

pub async fn overview(State(state): State<AppState>) -> impl IntoResponse {
    let events = match file_events::Entity::find()
        .filter(file_events::Column::At.gt(recent_cutoff()))
        .order_by_asc(file_events::Column::At)
        .all(&state.db)
        .await
    {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Latest terminal session per student.
    let mut terminals: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    match sessions::Entity::find()
        .order_by_desc(sessions::Column::StartedAt)
        .all(&state.db)
        .await
    {
        Ok(rows) => {
            for s in rows {
                terminals
                    .entry(s.student.clone())
                    .or_insert((s.id.to_string(), s.status));
            }
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }

    // Bucket events per student, preserving chronological order.
    use std::collections::HashMap;
    let mut per_student: HashMap<String, Vec<file_events::Model>> = HashMap::new();
    for ev in events {
        per_student.entry(ev.student.clone()).or_default().push(ev);
    }

    let now = Utc::now().timestamp();
    let mut students: Vec<OverviewStudent> = Vec::new();

    for (student, evs) in per_student {
        let Some(last) = evs.last() else { continue };
        let current_exercise = last.exercise.clone();

        // Accumulated active time on the current exercise, and when it started.
        let mut seconds_on_exercise = 0i64;
        let mut started_exercise: Option<i64> = None;
        for ev in &evs {
            if ev.exercise == current_exercise {
                let t = ev.at.timestamp_millis();
                started_exercise = Some(started_exercise.map_or(t, |s| s.min(t)));
            }
        }
        for pair in evs.windows(2) {
            let (cur, next) = (&pair[0], &pair[1]);
            if cur.kind == "close" || cur.exercise != current_exercise {
                continue;
            }
            let gap = (next.at.timestamp() - cur.at.timestamp()).clamp(0, IDLE_GAP_SECS);
            seconds_on_exercise += gap;
        }

        let last_seen = last.at.timestamp();
        let status = if now - last_seen <= ACTIVE_WINDOW_SECS {
            "active"
        } else {
            "idle"
        };
        let terminal = terminals.get(&student);

        students.push(OverviewStudent {
            student: student.clone(),
            file: last.relative_path.clone().or_else(|| Some(last.path.clone())),
            language: last.language.clone(),
            exercise: current_exercise,
            last_seen_unix_ms: last.at.timestamp_millis(),
            status: status.to_string(),
            seconds_on_exercise,
            started_exercise_unix_ms: started_exercise,
            terminal_session_id: terminal.map(|t| t.0.clone()),
            terminal_status: terminal.map(|t| t.1.clone()),
        });
    }

    // Group by current exercise.
    let mut groups: std::collections::BTreeMap<String, Vec<OverviewStudent>> =
        std::collections::BTreeMap::new();
    let mut no_exercise: Vec<OverviewStudent> = Vec::new();
    for s in students {
        match &s.exercise {
            Some(ex) => groups.entry(ex.clone()).or_default().push(s),
            None => no_exercise.push(s),
        }
    }

    let exercises: Vec<ExerciseGroup> = groups
        .into_iter()
        .map(|(exercise, mut students)| {
            students.sort_by(|a, b| b.seconds_on_exercise.cmp(&a.seconds_on_exercise));
            ExerciseGroup { exercise, students }
        })
        .collect();
    no_exercise.sort_by(|a, b| b.last_seen_unix_ms.cmp(&a.last_seen_unix_ms));

    Json(Overview {
        exercises,
        no_exercise,
    })
    .into_response()
}
