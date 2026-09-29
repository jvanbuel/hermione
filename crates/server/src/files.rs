//! File-activity ingest and analytics for the VSCode extension.
//!
//! The extension POSTs lightweight JSON events (focus changes, periodic
//! heartbeats, and coalesced edit bursts). We persist them and expose: the
//! latest activity per student (for live intervention) and time-on-task
//! aggregates (for offline analysis).

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use hermione_entity::{file_events, sessions};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::auth::AuthCtx;
use crate::error::ApiResult;
use crate::http::{resolve_course, CourseCtx, CourseQuery, VerifiedStudent};
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
    pub student_source: Option<String>,
    pub repo: Option<String>,
    pub kind: String,
    /// Document changes coalesced into an "edit" event. Absent from clients
    /// that predate edit reporting.
    pub edits: Option<i32>,
    /// 1-based cursor line, when the file was the one on screen.
    pub line: Option<i32>,
    pub at_unix_ms: i64,
}

/// Accepts a batch of file events from the extension. The course is determined
/// by the enrollment token (resolved into `CourseCtx` by the ingest gate).
/// Re-anchors a path that arrived absolute.
///
/// A notebook cell's URI is not workspace-resolvable, so older extensions
/// reported `/workspace/ex/nb.ipynb` — or, when the URI carried a remote
/// authority, `//codespaces+name/workspace/ex/nb.ipynb`. Both match no
/// exercise glob, so the work landed under no exercise at all.
///
/// The course's own exercise slugs say where the repo starts: the first
/// segment that names one is the anchor, and everything before it is the
/// machine's workspace root. Only absolute paths are touched — a correctly
/// reported path is relative and is left exactly as it is.
fn reanchor(relative: &str, slugs: &[String]) -> Option<(String, String)> {
    if !relative.starts_with('/') {
        return None;
    }
    let segments: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
    let at = segments
        .iter()
        .position(|seg| slugs.iter().any(|slug| slug == seg))?;
    Some((segments[at..].join("/"), segments[at].to_string()))
}

pub async fn ingest(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Json(events): Json<Vec<FileEventIn>>,
) -> ApiResult<(StatusCode, String)> {
    if events.is_empty() {
        return Ok((StatusCode::OK, "0".to_string()));
    }

    // Only worth a query when something actually needs rescuing.
    let slugs: Vec<String> = if events.iter().any(|e| {
        e.relative_path
            .as_deref()
            .is_some_and(|r| r.starts_with('/'))
    }) {
        crate::exercises::list_for_course(&state.db, course_id)
            .await?
            .into_iter()
            .map(|r| r.slug)
            .collect()
    } else {
        Vec::new()
    };

    let models: Vec<file_events::ActiveModel> = events
        .into_iter()
        .map(|mut e| {
            if let Some(rel) = e.relative_path.as_deref() {
                if let Some((fixed, exercise)) = reanchor(rel, &slugs) {
                    e.relative_path = Some(fixed);
                    // Keep whatever the client managed to work out; only fill
                    // in what it could not.
                    e.exercise = e.exercise.or(Some(exercise));
                }
            }
            e
        })
        .map(|e| file_events::ActiveModel {
            course_id: Set(Some(course_id)),
            // A verified identity (when enforced) overrides the self-asserted one.
            student: Set(verified.clone().unwrap_or(e.student)),
            workspace: Set(e.workspace),
            path: Set(e.path),
            relative_path: Set(e.relative_path),
            language: Set(e.language),
            exercise: Set(e.exercise),
            student_source: Set(e.student_source),
            repo: Set(e.repo),
            kind: Set(e.kind),
            edits: Set(e.edits),
            line: Set(e.line),
            at: Set(unix_ms_to_dt(e.at_unix_ms)),
            created_at: Set(Utc::now().into()),
            ..Default::default()
        })
        .collect();

    let count = models.len();
    file_events::Entity::insert_many(models)
        .exec(&state.db)
        .await?;
    Ok((StatusCode::OK, count.to_string()))
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

/// Latest file activity per student in a course — what each has open right now.
pub async fn students_activity(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> ApiResult<impl IntoResponse> {
    let course_id = resolve_course(&state, ctx, q.course).await?;
    let rows = file_events::Entity::find()
        .filter(file_events::Column::CourseId.eq(course_id))
        .filter(file_events::Column::At.gt(recent_cutoff()))
        .order_by_desc(file_events::Column::At)
        .all(&state.db)
        .await?;

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

    Ok(Json(latest))
}

#[derive(Deserialize)]
pub struct AnalyticsQuery {
    student: String,
    course: Option<String>,
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
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<AnalyticsQuery>,
) -> ApiResult<impl IntoResponse> {
    let course_id = resolve_course(&state, ctx, q.course).await?;
    let rows = file_events::Entity::find()
        .filter(file_events::Column::CourseId.eq(course_id))
        .filter(file_events::Column::Student.eq(&q.student))
        .order_by_asc(file_events::Column::At)
        .all(&state.db)
        .await?;

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

        let entry = per_file_secs.entry(cur.path.clone()).or_insert((
            cur.relative_path.clone(),
            cur.exercise.clone(),
            0,
        ));
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
    per_file.sort_by_key(|f| std::cmp::Reverse(f.seconds));

    let mut per_exercise: Vec<ExerciseTime> = per_exercise_secs
        .into_iter()
        .map(|(exercise, seconds)| ExerciseTime { exercise, seconds })
        .collect();
    per_exercise.sort_by_key(|e| std::cmp::Reverse(e.seconds));

    Ok(Json(TimeReport {
        student: q.student,
        total_seconds: total,
        per_file,
        per_exercise,
    }))
}

#[cfg(test)]
mod tests {

    /// The three shapes seen from one class, all for the same file.
    #[test]
    fn reanchors_the_paths_notebook_cells_produce() {
        let slugs = vec!["3-basic-transforms".to_string(), "5-joins".to_string()];

        // A correctly reported path is relative and must be left alone.
        assert_eq!(reanchor("3-basic-transforms/solution.ipynb", &slugs), None);

        // A cell URI with no authority.
        assert_eq!(
            reanchor("/workspace/3-basic-transforms/solution.ipynb", &slugs),
            Some((
                "3-basic-transforms/solution.ipynb".to_string(),
                "3-basic-transforms".to_string()
            ))
        );

        // A cell URI carrying a remote authority, rendered UNC-style.
        assert_eq!(
            reanchor(
                "//codespaces+curly-fishstick-96jg4p5x47q9h699/workspace/3-basic-transforms/solution.ipynb",
                &slugs
            ),
            Some((
                "3-basic-transforms/solution.ipynb".to_string(),
                "3-basic-transforms".to_string()
            ))
        );

        // Nothing recognisable: better untouched than mangled.
        assert_eq!(reanchor("/workspace/docs/theme/styles/x.css", &slugs), None);

        // A course with no exercises defined can rescue nothing.
        assert_eq!(reanchor("/workspace/3-basic-transforms/x.ipynb", &[]), None);
    }
    use super::*;

    fn stalled_for(secs: i64) -> EditActivity {
        EditActivity {
            recent: 0,
            since_last_secs: Some(secs),
        }
    }

    #[test]
    fn typing_students_are_not_flagged() {
        let edits = EditActivity {
            recent: 40,
            since_last_secs: Some(5),
        };
        let (level, _) = assess(Signals::default(), WATCH_SECS + 60, edits, Fences::FIXED);
        // Long time on the exercise still warrants a look, but steady typing
        // must not escalate it.
        assert_eq!(level, "watch");
    }

    #[test]
    fn silence_on_a_long_exercise_is_a_stall() {
        let (level, reasons) = assess(
            Signals::default(),
            WATCH_SECS + 60,
            stalled_for(STALL_SECS + 60),
            Fences::FIXED,
        );
        assert_eq!(level, "watch");
        assert!(reasons.iter().any(|r| r.starts_with("no edits for")));
    }

    #[test]
    fn a_stall_plus_a_failure_escalates_to_help() {
        let sig = Signals {
            errors: 1,
            failed_runs: 0,
        };
        let (level, _) = assess(
            sig,
            WATCH_SECS + 60,
            stalled_for(STALL_SECS + 60),
            Fences::FIXED,
        );
        assert_eq!(level, "help");
    }

    #[test]
    fn clients_without_edit_reporting_never_stall() {
        // `since_last_secs: None` is what an older extension produces; it must
        // read exactly as it did before the signal existed.
        let quiet = EditActivity::default();
        let (with, reasons) = assess(Signals::default(), WATCH_SECS + 60, quiet, Fences::FIXED);
        let (without, _) = assess(
            Signals::default(),
            WATCH_SECS + 60,
            EditActivity::default(),
            Fences::FIXED,
        );
        assert_eq!(with, without);
        assert!(!reasons.iter().any(|r| r.starts_with("no edits")));
    }

    #[test]
    fn a_stall_early_in_an_exercise_is_not_a_signal() {
        // Thinking for five minutes at the start of a problem is normal.
        let (level, _) = assess(
            Signals::default(),
            60,
            stalled_for(STALL_SECS + 60),
            Fences::FIXED,
        );
        assert_eq!(level, "ok");
    }

    const MIN: i64 = 60;

    #[test]
    fn a_small_cohort_falls_back_to_fixed_thresholds() {
        assert_eq!(Fences::for_cohort(&[60, 90, 120]), Fences::FIXED);
    }

    #[test]
    fn thresholds_follow_the_class_not_a_fixed_clock() {
        // A hard exercise: everyone takes 30-45 minutes. 27 minutes is quick.
        let hard = Fences::for_cohort(&[30 * MIN, 34 * MIN, 38 * MIN, 41 * MIN, 45 * MIN]);
        assert!(hard.watch > 45 * MIN, "{hard:?}");
        let (level, _) = assess(Signals::default(), 40 * MIN, EditActivity::default(), hard);
        assert_eq!(level, "ok", "a normal time for this exercise is not a flag");

        // An easy one: everyone is done in 6-9 minutes. 20 minutes stands out.
        let easy = Fences::for_cohort(&[6 * MIN, 7 * MIN, 7 * MIN, 8 * MIN, 9 * MIN]);
        let (level, reasons) = assess(Signals::default(), 20 * MIN, EditActivity::default(), easy);
        assert_eq!(level, "help");
        assert!(
            reasons.iter().any(|r| r.contains("class median 7 min")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_tight_class_does_not_flag_a_few_extra_minutes() {
        let tight = Fences::for_cohort(&[2 * MIN, 2 * MIN, 2 * MIN, 2 * MIN, 2 * MIN]);
        assert_eq!((tight.watch, tight.help), (MIN_WATCH_SECS, MIN_HELP_SECS));
        let (level, _) = assess(Signals::default(), 4 * MIN, EditActivity::default(), tight);
        assert_eq!(level, "ok");
    }

    #[test]
    fn a_slow_student_who_is_typing_is_watched_not_helped() {
        let f = Fences::for_cohort(&[8 * MIN, 9 * MIN, 10 * MIN, 10 * MIN, 12 * MIN]);
        let typing = EditActivity {
            recent: 30,
            since_last_secs: Some(10),
        };
        let (level, _) = assess(Signals::default(), f.help + MIN, typing, f);
        assert_eq!(level, "watch");
        let (level, _) = assess(
            Signals::default(),
            f.help + MIN,
            stalled_for(STALL_SECS + 60),
            f,
        );
        assert_eq!(level, "help");
    }

    #[test]
    fn quantiles_interpolate() {
        assert_eq!(quantile(&[10, 20, 30, 40], 0.5), 25);
        assert_eq!(quantile(&[10, 20, 30, 40], 0.25), 18);
        assert_eq!(quantile(&[7], 0.75), 7);
    }
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
    /// How the identity was derived, and the repo it came from (provenance).
    student_source: Option<String>,
    repo: Option<String>,
    /// Struggle assessment: "ok", "watch", or "help".
    struggle: String,
    /// Human-readable reasons behind the struggle level.
    struggle_reasons: Vec<String>,
    /// Error-like outputs across the student's recent sessions.
    errors: i32,
    /// Recent sessions that exited non-zero.
    failed_runs: i32,
    /// Document changes in the last `EDIT_WINDOW_SECS` — whether they're typing.
    edits_recent: i32,
    /// When the student last typed, if their client reports edits at all.
    last_edit_unix_ms: Option<i64>,
}

/// Fixed time-on-exercise thresholds (seconds). Only used when a cohort is too
/// small to say what "long" means for an exercise; otherwise the thresholds are
/// derived from how the class itself is doing (see `Fences::for_cohort`).
const WATCH_SECS: i64 = 10 * 60;
const HELP_SECS: i64 = 25 * 60;

/// Fewest students needed on an exercise before its own distribution is trusted.
const MIN_COHORT: usize = 5;

/// However tightly a class clusters, nobody is an outlier before this long.
const MIN_WATCH_SECS: i64 = 5 * 60;
const MIN_HELP_SECS: i64 = 10 * 60;

/// Where "unusually long on this exercise" starts, for one exercise.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fences {
    watch: i64,
    help: i64,
    /// Students the thresholds were derived from; 0 when they are the fixed ones.
    cohort: usize,
    /// Median time on the exercise across that cohort (seconds).
    median: i64,
}

impl Fences {
    const FIXED: Fences = Fences {
        watch: WATCH_SECS,
        help: HELP_SECS,
        cohort: 0,
        median: 0,
    };

    /// Outliers of the distribution of time spent on one exercise, counting
    /// everyone who has spent time on it — including students who moved on, whose
    /// finished times say how long the exercise takes.
    ///
    /// Tukey's fences: past `Q3 + 1.5·IQR` is a mild outlier (watch), past
    /// `Q3 + 3·IQR` an extreme one (help). Times are right-skewed and a tight
    /// class has a tiny IQR, so each fence is also held to a multiple of the
    /// median and to a floor, or a few extra minutes would flag someone.
    fn for_cohort(times: &[i64]) -> Fences {
        if times.len() < MIN_COHORT {
            return Fences::FIXED;
        }
        let mut t = times.to_vec();
        t.sort_unstable();
        let (q1, med, q3) = (quantile(&t, 0.25), quantile(&t, 0.5), quantile(&t, 0.75));
        let iqr = q3 - q1;
        Fences {
            watch: (q3 + iqr * 3 / 2).max(med * 3 / 2).max(MIN_WATCH_SECS),
            help: (q3 + iqr * 3).max(med * 2).max(MIN_HELP_SECS),
            cohort: t.len(),
            median: med,
        }
    }
}

/// Linear-interpolated quantile of an ascending, non-empty slice.
fn quantile(sorted: &[i64], q: f64) -> i64 {
    let pos = q * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    let frac = pos - lo as f64;
    (sorted[lo] as f64 + (sorted[hi] - sorted[lo]) as f64 * frac).round() as i64
}

/// A present student who has been on the same exercise a while but hasn't typed
/// for this long is more likely stuck than working.
const STALL_SECS: i64 = 5 * 60;

/// Window over which recent edit volume is summed for the dashboard.
const EDIT_WINDOW_SECS: i64 = 5 * 60;

/// Per-student error/failure tallies from recent terminal sessions.
#[derive(Default, Clone, Copy)]
struct Signals {
    errors: i32,
    failed_runs: i32,
}

/// How much the student has actually been typing, as opposed to how long the
/// file has been on screen.
#[derive(Default, Clone, Copy)]
struct EditActivity {
    /// Document changes reported within `EDIT_WINDOW_SECS`.
    recent: i32,
    /// Seconds since the last reported edit. `None` when the signal cannot be
    /// trusted — the student is away, or their client never reports edits.
    since_last_secs: Option<i64>,
}

fn struggle_rank(level: &str) -> u8 {
    match level {
        "help" => 2,
        "watch" => 1,
        _ => 0,
    }
}

/// Combines signals into a struggle level and the reasons for it.
fn assess(
    sig: Signals,
    seconds_on_exercise: i64,
    edits: EditActivity,
    fences: Fences,
) -> (String, Vec<String>) {
    let mut reasons = Vec::new();
    if sig.errors > 0 {
        reasons.push(format!(
            "{} error{} in recent runs",
            sig.errors,
            if sig.errors == 1 { "" } else { "s" }
        ));
    }
    if sig.failed_runs > 0 {
        reasons.push(format!(
            "{} failed run{}",
            sig.failed_runs,
            if sig.failed_runs == 1 { "" } else { "s" }
        ));
    }
    let long = seconds_on_exercise >= fences.watch;
    if long {
        let mins = seconds_on_exercise / 60;
        reasons.push(if fences.cohort > 0 {
            format!("{mins} min, class median {} min", fences.median / 60)
        } else {
            format!("{mins} min on this exercise")
        });
    }

    // Time-on-task alone is ambiguous: a student steadily writing code for 20
    // minutes looks identical to one who has been staring at the same screen.
    // A long stretch with no typing is what separates them.
    let stalled = long && matches!(edits.since_last_secs, Some(s) if s >= STALL_SECS);
    if let (true, Some(s)) = (stalled, edits.since_last_secs) {
        reasons.push(format!("no edits for {} min", s / 60));
    }
    // Slow but visibly working: an outlier on the clock, not necessarily stuck.
    let working = edits.recent > 0 && matches!(edits.since_last_secs, Some(s) if s < STALL_SECS);

    let help = sig.errors >= 3
        || sig.failed_runs >= 2
        || (seconds_on_exercise >= fences.help && !working)
        || (stalled && (sig.errors >= 1 || sig.failed_runs >= 1));
    let watch = stalled || sig.errors >= 1 || sig.failed_runs >= 1 || long;
    let level = if help {
        "help"
    } else if watch {
        "watch"
    } else {
        "ok"
    };
    (level.to_string(), reasons)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExerciseGroup {
    exercise: String,
    title: String,
    students: Vec<OverviewStudent>,
    stats: GroupStats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupStats {
    total: usize,
    active: usize,
    need_help: usize,
    /// Median time-on-exercise across the group (seconds).
    median_seconds: i64,
    /// Time-on-exercise past which a student is an outlier for this exercise.
    watch_secs: i64,
    help_secs: i64,
    /// Students the thresholds came from; 0 means the fixed fallback.
    cohort_size: usize,
}

fn group_stats(students: &[OverviewStudent], fences: Fences) -> GroupStats {
    let mut secs: Vec<i64> = students.iter().map(|s| s.seconds_on_exercise).collect();
    secs.sort_unstable();
    let median = if secs.is_empty() {
        0
    } else {
        secs[secs.len() / 2]
    };
    GroupStats {
        total: students.len(),
        active: students.iter().filter(|s| s.status == "active").count(),
        need_help: students.iter().filter(|s| s.struggle == "help").count(),
        median_seconds: median,
        watch_secs: fences.watch,
        help_secs: fences.help,
        cohort_size: fences.cohort,
    }
}

fn into_group(
    exercise: String,
    title: String,
    mut students: Vec<OverviewStudent>,
    fences: Fences,
) -> ExerciseGroup {
    // Struggling students float to the top, then by time-on-exercise.
    students.sort_by(|a, b| {
        struggle_rank(&b.struggle)
            .cmp(&struggle_rank(&a.struggle))
            .then(b.seconds_on_exercise.cmp(&a.seconds_on_exercise))
    });
    let stats = group_stats(&students, fences);
    ExerciseGroup {
        exercise,
        title,
        students,
        stats,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    exercises: Vec<ExerciseGroup>,
    /// Students whose current file maps to no exercise.
    no_exercise: Vec<OverviewStudent>,
}

pub async fn overview(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> ApiResult<impl IntoResponse> {
    let course_id = resolve_course(&state, ctx, q.course).await?;
    // The three reads are independent; this endpoint is polled, so run them
    // together rather than one after another.
    let recent = recent_cutoff();
    let (events, recent_sessions, defined) = tokio::join!(
        file_events::Entity::find()
            .filter(file_events::Column::CourseId.eq(course_id))
            .filter(file_events::Column::At.gt(recent))
            .order_by_asc(file_events::Column::At)
            .all(&state.db),
        // Only the recent window (the same one used for file events), so the
        // scan stays bounded as session history grows — and the dashboard is a
        // live view of the current teaching session anyway.
        sessions::Entity::find()
            .filter(sessions::Column::CourseId.eq(course_id))
            .filter(sessions::Column::StartedAt.gt(recent))
            .order_by_desc(sessions::Column::StartedAt)
            .all(&state.db),
        crate::exercises::list_for_course(&state.db, course_id),
    );
    let events = events?;
    let recent_sessions = recent_sessions?;
    let defined = defined?;

    // Latest terminal session per student (within this course), plus struggle
    // signals (errors / failed runs) from each student's recent sessions.
    let mut terminals: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut signals: std::collections::HashMap<String, Signals> = std::collections::HashMap::new();
    for s in recent_sessions {
        terminals
            .entry(s.student.clone())
            .or_insert((s.id.to_string(), s.status.clone()));
        let sig = signals.entry(s.student.clone()).or_default();
        sig.errors += s.error_count;
        if matches!(s.exit_code, Some(code) if code != 0) {
            sig.failed_runs += 1;
        }
    }

    // Bucket events per student, preserving chronological order.
    use std::collections::HashMap;
    let mut per_student: HashMap<String, Vec<file_events::Model>> = HashMap::new();
    for ev in events {
        per_student.entry(ev.student.clone()).or_default().push(ev);
    }

    // What "long" means depends on the exercise, so measure each one against the
    // whole class's time on it — not only the students on it right now.
    let mut cohort_times: HashMap<String, Vec<i64>> = HashMap::new();
    for evs in per_student.values() {
        let mut per_exercise: HashMap<&str, i64> = HashMap::new();
        for pair in evs.windows(2) {
            let (cur, next) = (&pair[0], &pair[1]);
            let Some(ex) = cur.exercise.as_deref() else {
                continue;
            };
            if cur.kind == "close" {
                continue;
            }
            *per_exercise.entry(ex).or_default() +=
                (next.at.timestamp() - cur.at.timestamp()).clamp(0, IDLE_GAP_SECS);
        }
        for (ex, secs) in per_exercise {
            if secs > 0 {
                cohort_times.entry(ex.to_string()).or_default().push(secs);
            }
        }
    }
    let fences: HashMap<String, Fences> = cohort_times
        .into_iter()
        .map(|(ex, times)| (ex, Fences::for_cohort(&times)))
        .collect();
    let fences_for = |ex: &str| fences.get(ex).copied().unwrap_or(Fences::FIXED);

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

        // Typing activity, scoped to the exercise the student is on now — the
        // same scope as `seconds_on_exercise`, so the two are comparable.
        // Edits on a previous exercise say nothing about being stuck on this
        // one. `last_edit` stays `None` for clients that never send edit
        // events, which keeps students on an older extension from all looking
        // stalled.
        let mut edits_recent = 0i32;
        let mut last_edit: Option<i64> = None;
        for ev in &evs {
            if ev.kind != "edit" || ev.exercise != current_exercise {
                continue;
            }
            let t = ev.at.timestamp();
            last_edit = Some(last_edit.map_or(t, |prev: i64| prev.max(t)));
            if now - t <= EDIT_WINDOW_SECS {
                edits_recent += ev.edits.unwrap_or(0);
            }
        }
        let edit_activity = EditActivity {
            recent: edits_recent,
            // Someone who walked away isn't stuck, they're gone — so the
            // "hasn't typed" signal only counts while they're present.
            since_last_secs: match (status, last_edit) {
                ("active", Some(t)) => Some(now - t),
                _ => None,
            },
        };

        let terminal = terminals.get(&student);
        let sig = signals.get(&student).copied().unwrap_or_default();
        let (struggle, struggle_reasons) = assess(
            sig,
            seconds_on_exercise,
            edit_activity,
            current_exercise
                .as_deref()
                .map_or(Fences::FIXED, &fences_for),
        );

        students.push(OverviewStudent {
            student: student.clone(),
            file: last
                .relative_path
                .clone()
                .or_else(|| Some(last.path.clone())),
            language: last.language.clone(),
            exercise: current_exercise,
            last_seen_unix_ms: last.at.timestamp_millis(),
            status: status.to_string(),
            seconds_on_exercise,
            started_exercise_unix_ms: started_exercise,
            terminal_session_id: terminal.map(|t| t.0.clone()),
            terminal_status: terminal.map(|t| t.1.clone()),
            student_source: last.student_source.clone(),
            repo: last.repo.clone(),
            struggle,
            struggle_reasons,
            errors: sig.errors,
            failed_runs: sig.failed_runs,
            edits_recent: edit_activity.recent,
            last_edit_unix_ms: last_edit.map(|t| t * 1000),
        });
    }

    // Bucket students by their current exercise slug.
    let mut groups: std::collections::HashMap<String, Vec<OverviewStudent>> = HashMap::new();
    let mut no_exercise: Vec<OverviewStudent> = Vec::new();
    for s in students {
        match &s.exercise {
            Some(ex) => groups.entry(ex.clone()).or_default().push(s),
            None => no_exercise.push(s),
        }
    }

    // Emit defined exercises first, in their configured order (including ones
    // nobody has started yet), then any active-but-undefined exercises.
    let mut output: Vec<ExerciseGroup> = Vec::new();
    for ex in defined {
        let students = groups.remove(&ex.slug).unwrap_or_default();
        let f = fences_for(&ex.slug);
        output.push(into_group(ex.slug, ex.title, students, f));
    }
    let mut leftover: Vec<(String, Vec<OverviewStudent>)> = groups.into_iter().collect();
    leftover.sort_by(|a, b| a.0.cmp(&b.0));
    for (slug, students) in leftover {
        let title = slug.clone();
        let f = fences_for(&slug);
        output.push(into_group(slug, title, students, f));
    }

    no_exercise.sort_by_key(|s| std::cmp::Reverse(s.last_seen_unix_ms));

    Ok(Json(Overview {
        exercises: output,
        no_exercise,
    }))
}
