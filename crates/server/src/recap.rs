//! The class recap: one lesson, summed up for the teacher who ran it.
//!
//! The live board answers "who needs me now?". This answers, afterwards, "how did
//! that go, who do I follow up with, and what do I re-teach?" — from the same
//! file events and terminal sessions, judged by the same rules: the board's
//! per-exercise thresholds decide who needed help, so the recap can't disagree
//! with what the teacher saw at the time.
//!
//! Everything here is a plain function over plain data, so it is tested without
//! a database; [`handler`] is the thin part that fetches the rows.

use std::collections::HashMap;

use axum::{
    extract::{Extension, Query, State},
    Json,
};
use hermione_entity::{file_events, messages, sessions};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use crate::auth::AuthCtx;
use crate::error::ApiResult;
use crate::files::{assess, quantile, EditActivity, Fences, Signals, IDLE_GAP_SECS};
use crate::http::{resolve_course, CourseQuery};
use crate::state::AppState;

/// Activity separated by more than this is a different lesson.
const LESSON_GAP_SECS: i64 = 30 * 60;
/// How far back lessons are looked for.
const LOOKBACK_DAYS: i64 = 14;
/// Lessons offered for navigation.
const MAX_LESSONS: usize = 12;
/// Width of one timeline bar.
const BUCKET_SECS: i64 = 5 * 60;
/// Less active time than this and a student was barely part of the lesson.
const BARELY_THERE_SECS: i64 = 5 * 60;
/// Students "to watch" listed beside everyone who needed help.
const MAX_WATCH_LISTED: usize = 10;

// --- what the aggregation reads -----------------------------------------------

/// One file event, reduced to what the recap uses. Times are seconds.
#[derive(Clone)]
pub struct Ev {
    pub student: String,
    pub exercise: Option<String>,
    pub closed: bool,
    pub edits: i32,
    pub at: i64,
}

/// Errors and failed runs a student's terminal produced during the lesson.
pub struct Trouble {
    pub student: String,
    pub errors: i32,
    pub failed_runs: i32,
}

/// A message the teacher sent during the lesson.
pub struct Sent {
    pub at: i64,
    pub text: String,
    /// `None` is everyone.
    pub to: Option<String>,
}

pub struct ExerciseInfo {
    pub slug: String,
    pub title: String,
}

/// A stretch of time, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: i64,
    pub to: i64,
}

// --- what it answers with -------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recap {
    from_unix_ms: i64,
    to_unix_ms: i64,
    /// Students with any activity in the lesson.
    students: usize,
    /// Median active time across them (seconds).
    median_active_seconds: i64,
    exercises: Vec<ExerciseRecap>,
    follow_up: Vec<FollowUp>,
    quiet: Vec<Quiet>,
    timeline: Vec<Bucket>,
    messages: Vec<SentOut>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExerciseRecap {
    exercise: String,
    title: String,
    students: usize,
    median_seconds: i64,
    longest_seconds: i64,
    /// Students who, by the board's own rules, needed help on it.
    needed_help: usize,
    to_watch: usize,
    /// Students whose last activity was on a later exercise.
    moved_on: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FollowUp {
    student: String,
    exercise: String,
    /// "help" or "watch".
    level: &'static str,
    seconds: i64,
    reasons: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Quiet {
    student: String,
    /// "barely there" or "never typed".
    why: &'static str,
    active_seconds: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bucket {
    at_unix_ms: i64,
    /// Students with any activity in this stretch.
    active: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SentOut {
    at_unix_ms: i64,
    text: String,
    to: Option<String>,
}

// --- lessons ---------------------------------------------------------------------

/// Lessons in a course's activity: runs of events with no gap of `gap` or more.
/// `times` are seconds, newest first; the result is newest first too.
pub fn lessons(times_desc: &[i64], gap: i64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for &t in times_desc {
        match out.last_mut() {
            Some(open) if open.from - t < gap => open.from = t,
            _ => out.push(Span { from: t, to: t }),
        }
    }
    out
}

// --- the recap -------------------------------------------------------------------

#[derive(Default)]
struct PerStudent {
    /// Seconds per exercise index, and outside any exercise.
    by_exercise: HashMap<usize, i64>,
    total: i64,
    edits: i64,
    /// Exercise index of the student's latest activity that was on an exercise.
    last_exercise: Option<usize>,
}

/// Sums up a lesson. `events` must be in time order.
pub fn build(
    window: Span,
    events: &[Ev],
    trouble: &[Trouble],
    sent: &[Sent],
    exercises: &[ExerciseInfo],
) -> Recap {
    // Exercises in course order, then any that activity mentions but the course
    // never defined.
    let mut order: Vec<(String, String)> = exercises
        .iter()
        .map(|e| (e.slug.clone(), e.title.clone()))
        .collect();
    let mut undefined: Vec<&str> = events
        .iter()
        .filter_map(|e| e.exercise.as_deref())
        .filter(|slug| !order.iter().any(|(s, _)| s == slug))
        .collect();
    undefined.sort_unstable();
    undefined.dedup();
    order.extend(
        undefined
            .into_iter()
            .map(|s| (s.to_string(), s.to_string())),
    );
    let index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, (slug, _))| (slug.as_str(), i))
        .collect();

    // Time is credited to the file a student had open until their next event,
    // the gap capped so a lunch break isn't counted as work — as Analytics does.
    let mut by_student: HashMap<&str, Vec<&Ev>> = HashMap::new();
    for e in events {
        by_student.entry(e.student.as_str()).or_default().push(e);
    }
    let mut per: HashMap<&str, PerStudent> = HashMap::new();
    for (student, evs) in &by_student {
        let mut p = PerStudent::default();
        for (i, e) in evs.iter().enumerate() {
            p.edits += i64::from(e.edits.max(0));
            let ex = e.exercise.as_deref().and_then(|s| index.get(s)).copied();
            if let Some(ex) = ex {
                p.last_exercise = Some(ex);
            }
            let Some(next) = evs.get(i + 1) else { continue };
            if e.closed {
                continue;
            }
            let gap = (next.at - e.at).clamp(0, IDLE_GAP_SECS);
            p.total += gap;
            if let Some(ex) = ex {
                *p.by_exercise.entry(ex).or_default() += gap;
            }
        }
        per.insert(student, p);
    }

    // What "long" means on each exercise comes from how this class did on it.
    let mut fences: Vec<Fences> = Vec::with_capacity(order.len());
    for ex in 0..order.len() {
        let times: Vec<i64> = per
            .values()
            .filter_map(|p| p.by_exercise.get(&ex).copied())
            .filter(|&t| t > 0)
            .collect();
        fences.push(Fences::for_cohort(&times));
    }

    // Terminal trouble is per student, not per exercise: it is held against the
    // exercise they spent most time on, rather than smeared over all of them.
    let trouble: HashMap<&str, Signals> = trouble
        .iter()
        .map(|t| {
            (
                t.student.as_str(),
                Signals {
                    errors: t.errors,
                    failed_runs: t.failed_runs,
                },
            )
        })
        .collect();

    let mut recaps: Vec<ExerciseRecap> = order
        .iter()
        .map(|(slug, title)| ExerciseRecap {
            exercise: slug.clone(),
            title: title.clone(),
            students: 0,
            median_seconds: 0,
            longest_seconds: 0,
            needed_help: 0,
            to_watch: 0,
            moved_on: 0,
        })
        .collect();
    let mut follow_up: Vec<FollowUp> = Vec::new();
    for (student, p) in &per {
        let top = p
            .by_exercise
            .iter()
            .max_by_key(|(_, &t)| t)
            .map(|(&ex, _)| ex);
        for (&ex, &secs) in p.by_exercise.iter().filter(|(_, &t)| t > 0) {
            let signals = if Some(ex) == top {
                trouble.get(student).copied().unwrap_or_default()
            } else {
                Signals::default()
            };
            let (level, reasons) = assess(signals, secs, EditActivity::default(), fences[ex]);
            let r = &mut recaps[ex];
            r.students += 1;
            r.longest_seconds = r.longest_seconds.max(secs);
            if p.last_exercise.is_some_and(|last| last > ex) {
                r.moved_on += 1;
            }
            let level: &'static str = match level.as_str() {
                "help" => {
                    r.needed_help += 1;
                    "help"
                }
                "watch" => {
                    r.to_watch += 1;
                    "watch"
                }
                _ => continue,
            };
            follow_up.push(FollowUp {
                student: (*student).to_string(),
                exercise: order[ex].0.clone(),
                level,
                seconds: secs,
                reasons,
            });
        }
    }
    // Medians, from the same per-student times the fences used.
    for (ex, r) in recaps.iter_mut().enumerate() {
        let mut t: Vec<i64> = per
            .values()
            .filter_map(|p| p.by_exercise.get(&ex).copied())
            .filter(|&t| t > 0)
            .collect();
        t.sort_unstable();
        r.median_seconds = if t.is_empty() { 0 } else { quantile(&t, 0.5) };
    }
    // Exercises nobody worked on are not part of this lesson.
    recaps.retain(|r| r.students > 0);

    // Everyone who helped, then those worth watching; longest first within each.
    follow_up.sort_by(|a, b| {
        (b.level == "help")
            .cmp(&(a.level == "help"))
            .then(b.seconds.cmp(&a.seconds))
            .then(a.student.cmp(&b.student))
    });
    let mut watching = 0;
    follow_up.retain(|f| {
        if f.level == "help" {
            return true;
        }
        watching += 1;
        watching <= MAX_WATCH_LISTED
    });

    // Present but silent. A client that never reports edits would make everyone
    // "never typed", so that is only said when somebody in the class did type.
    let class_typed = per.values().any(|p| p.edits > 0);
    let mut quiet: Vec<Quiet> = per
        .iter()
        .filter_map(|(student, p)| {
            let why = if p.total < BARELY_THERE_SECS {
                "barely there"
            } else if class_typed && p.edits == 0 {
                "never typed"
            } else {
                return None;
            };
            Some(Quiet {
                student: (*student).to_string(),
                why,
                active_seconds: p.total,
            })
        })
        .collect();
    quiet.sort_by(|a, b| {
        a.active_seconds
            .cmp(&b.active_seconds)
            .then(a.student.cmp(&b.student))
    });

    let mut totals: Vec<i64> = per.values().map(|p| p.total).collect();
    totals.sort_unstable();

    Recap {
        from_unix_ms: window.from * 1000,
        to_unix_ms: window.to * 1000,
        students: per.len(),
        median_active_seconds: if totals.is_empty() {
            0
        } else {
            quantile(&totals, 0.5)
        },
        exercises: recaps,
        follow_up,
        quiet,
        timeline: timeline(window, events),
        messages: sent
            .iter()
            .map(|m| SentOut {
                at_unix_ms: m.at * 1000,
                text: m.text.clone(),
                to: m.to.clone(),
            })
            .collect(),
    }
}

/// How many students were active in each five minutes of the lesson.
fn timeline(window: Span, events: &[Ev]) -> Vec<Bucket> {
    let start = window.from - window.from.rem_euclid(BUCKET_SECS);
    let n = ((window.to - start) / BUCKET_SECS + 1) as usize;
    let mut seen: Vec<std::collections::HashSet<&str>> = vec![Default::default(); n];
    for e in events.iter().filter(|e| !e.closed) {
        let i = ((e.at - start) / BUCKET_SECS) as usize;
        if let Some(bucket) = seen.get_mut(i) {
            bucket.insert(e.student.as_str());
        }
    }
    seen.into_iter()
        .enumerate()
        .map(|(i, s)| Bucket {
            at_unix_ms: (start + i as i64 * BUCKET_SECS) * 1000,
            active: s.len(),
        })
        .collect()
}

// --- the endpoint ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RecapQuery {
    #[serde(flatten)]
    course: CourseQuery,
    /// Which lesson: 0 is the latest, 1 the one before, and so on.
    lesson: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecapReply {
    /// The lessons on offer, newest first.
    lessons: Vec<LessonSpan>,
    /// Which of them this recap is of; `None` when the course has no activity.
    index: Option<usize>,
    recap: Option<Recap>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LessonSpan {
    from_unix_ms: i64,
    to_unix_ms: i64,
}

/// GET /api/recap?course=…&lesson=0 — a lesson summed up.
pub async fn handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<RecapQuery>,
) -> ApiResult<Json<RecapReply>> {
    let course_id = resolve_course(&state, ctx, q.course.course).await?;

    // Every event time in the lookback, newest first, is enough to find where
    // lessons begin and end without reading the events themselves.
    let since = chrono::Utc::now() - chrono::Duration::days(LOOKBACK_DAYS);
    let times: Vec<i64> = file_events::Entity::find()
        .select_only()
        .column(file_events::Column::At)
        .filter(file_events::Column::CourseId.eq(course_id))
        .filter(file_events::Column::At.gt(since))
        .order_by_desc(file_events::Column::At)
        .limit(200_000)
        .into_tuple::<chrono::DateTime<chrono::FixedOffset>>()
        .all(&state.db)
        .await?
        .into_iter()
        .map(|t| t.timestamp())
        .collect();
    let found = lessons(&times, LESSON_GAP_SECS);
    let offered: Vec<LessonSpan> = found
        .iter()
        .take(MAX_LESSONS)
        .map(|s| LessonSpan {
            from_unix_ms: s.from * 1000,
            to_unix_ms: s.to * 1000,
        })
        .collect();
    let index = q.lesson.unwrap_or(0);
    let Some(window) = found.get(index).copied().filter(|_| index < MAX_LESSONS) else {
        return Ok(Json(RecapReply {
            lessons: offered,
            index: None,
            recap: None,
        }));
    };

    let at = |secs: i64| {
        chrono::DateTime::from_timestamp(secs, 0)
            .unwrap_or_default()
            .fixed_offset()
    };
    let (from, to) = (at(window.from), at(window.to));
    let events: Vec<Ev> = file_events::Entity::find()
        .filter(file_events::Column::CourseId.eq(course_id))
        .filter(file_events::Column::At.gte(from))
        .filter(file_events::Column::At.lte(to))
        .order_by_asc(file_events::Column::At)
        .all(&state.db)
        .await?
        .into_iter()
        .map(|e| Ev {
            student: e.student,
            exercise: e.exercise,
            closed: e.kind == "close",
            edits: e.edits.unwrap_or(0),
            at: e.at.timestamp(),
        })
        .collect();

    let mut trouble: HashMap<String, Trouble> = HashMap::new();
    for s in sessions::Entity::find()
        .filter(sessions::Column::CourseId.eq(course_id))
        .filter(sessions::Column::StartedAt.gte(from))
        .filter(sessions::Column::StartedAt.lte(to))
        .all(&state.db)
        .await?
    {
        let t = trouble.entry(s.student.clone()).or_insert(Trouble {
            student: s.student,
            errors: 0,
            failed_runs: 0,
        });
        t.errors += s.error_count;
        if matches!(s.exit_code, Some(code) if code != 0) {
            t.failed_runs += 1;
        }
    }
    let trouble: Vec<Trouble> = trouble.into_values().collect();

    let sent: Vec<Sent> = messages::Entity::find()
        .filter(messages::Column::CourseId.eq(course_id))
        .filter(messages::Column::CreatedAt.gte(from))
        .filter(messages::Column::CreatedAt.lte(to))
        .order_by_asc(messages::Column::Id)
        .all(&state.db)
        .await?
        .into_iter()
        .map(|m| Sent {
            at: m.created_at.timestamp(),
            text: m.body,
            to: m.student,
        })
        .collect();

    let exercises: Vec<ExerciseInfo> = crate::exercises::list_for_course(&state.db, course_id)
        .await?
        .into_iter()
        .map(|e| ExerciseInfo {
            slug: e.slug,
            title: e.title,
        })
        .collect();

    // Summing up a lesson walks every event, so it is not done on the async
    // runtime's worker threads.
    let recap =
        tokio::task::spawn_blocking(move || build(window, &events, &trouble, &sent, &exercises))
            .await?;
    Ok(Json(RecapReply {
        lessons: offered,
        index: Some(index),
        recap: Some(recap),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60;

    fn ev(student: &str, exercise: &str, at_min: i64, edits: i32) -> Ev {
        Ev {
            student: student.into(),
            exercise: Some(exercise.into()),
            closed: false,
            edits,
            at: at_min * MIN,
        }
    }

    fn info(slug: &str) -> ExerciseInfo {
        ExerciseInfo {
            slug: slug.into(),
            title: format!("Title {slug}"),
        }
    }

    /// One event a minute for `mins` minutes on `exercise`, the last one a close.
    fn work(student: &str, exercise: &str, from_min: i64, mins: i64, edits: i32) -> Vec<Ev> {
        (0..mins)
            .map(|i| ev(student, exercise, from_min + i, edits))
            .collect()
    }

    fn span(events: &[Ev]) -> Span {
        Span {
            from: events.first().unwrap().at,
            to: events.last().unwrap().at,
        }
    }

    fn run(events: Vec<Ev>, trouble: &[Trouble], exercises: &[ExerciseInfo]) -> Recap {
        let mut events = events;
        events.sort_by_key(|e| e.at);
        build(span(&events), &events, trouble, &[], exercises)
    }

    #[test]
    fn a_gap_of_half_an_hour_splits_lessons_and_the_newest_comes_first() {
        let times = [
            200 * MIN,
            190 * MIN,
            165 * MIN,
            100 * MIN,
            99 * MIN,
            10 * MIN,
        ];
        let found = lessons(&times, LESSON_GAP_SECS);
        assert_eq!(
            found,
            [
                Span {
                    from: 165 * MIN,
                    to: 200 * MIN
                },
                Span {
                    from: 99 * MIN,
                    to: 100 * MIN
                },
                Span {
                    from: 10 * MIN,
                    to: 10 * MIN
                },
            ]
        );
        assert!(lessons(&[], LESSON_GAP_SECS).is_empty());
    }

    #[test]
    fn time_is_summed_per_exercise_with_idle_gaps_capped() {
        // Ten minutes of ex1, then a two-hour lunch, then one more event.
        let mut events = work("ada", "ex1", 0, 10, 1);
        events.push(ev("ada", "ex1", 130, 0));
        let r = run(events, &[], &[info("ex1")]);
        let ex = &r.exercises[0];
        // 9 minute-long gaps, plus the lunch counted only up to the idle cap.
        assert_eq!(ex.longest_seconds, 9 * 60 + IDLE_GAP_SECS);
        assert_eq!((ex.students, r.students), (1, 1));
    }

    #[test]
    fn who_needed_help_is_the_boards_own_judgement_of_the_class() {
        // Five students take ~8 minutes on ex1; one takes 40.
        let mut events = Vec::new();
        for name in ["a", "b", "c", "d", "e"] {
            events.extend(work(name, "ex1", 0, 9, 1));
        }
        events.extend(work("slow", "ex1", 0, 41, 1));
        let r = run(events, &[], &[info("ex1")]);
        let ex = &r.exercises[0];
        assert_eq!(ex.students, 6);
        assert!(ex.needed_help >= 1, "the outlier needed help");
        assert_eq!(r.follow_up[0].student, "slow");
        assert_eq!(r.follow_up[0].level, "help");
        assert!(r.follow_up[0]
            .reasons
            .iter()
            .any(|x| x.contains("class median")));
        // ...and nobody else was picked out.
        assert!(r
            .follow_up
            .iter()
            .skip(1)
            .all(|f| f.student == "slow" || f.level == "watch"));
    }

    #[test]
    fn terminal_trouble_counts_against_the_exercise_they_spent_most_time_on() {
        let mut events = work("ada", "ex1", 0, 3, 1);
        events.extend(work("ada", "ex2", 3, 8, 1)); // most time here
        let trouble = [Trouble {
            student: "ada".into(),
            errors: 4,
            failed_runs: 0,
        }];
        let r = run(events, &trouble, &[info("ex1"), info("ex2")]);
        let helped: Vec<_> = r
            .follow_up
            .iter()
            .map(|f| (f.student.as_str(), f.exercise.as_str()))
            .collect();
        assert_eq!(helped, [("ada", "ex2")]);
    }

    #[test]
    fn moved_on_means_their_last_activity_was_on_a_later_exercise() {
        let mut events = work("done", "ex1", 0, 5, 1);
        events.extend(work("done", "ex2", 5, 5, 1));
        events.extend(work("stuck", "ex1", 0, 10, 1));
        let r = run(events, &[], &[info("ex1"), info("ex2")]);
        let ex1 = r.exercises.iter().find(|e| e.exercise == "ex1").unwrap();
        assert_eq!((ex1.students, ex1.moved_on), (2, 1));
        let ex2 = r.exercises.iter().find(|e| e.exercise == "ex2").unwrap();
        assert_eq!(ex2.moved_on, 0);
    }

    #[test]
    fn exercises_come_in_course_order_and_undefined_ones_after() {
        let mut events = work("a", "zzz", 0, 3, 1);
        events.extend(work("a", "ex2", 3, 3, 1));
        events.extend(work("a", "ex1", 6, 3, 1));
        let r = run(events, &[], &[info("ex1"), info("ex2"), info("ex3")]);
        let names: Vec<_> = r.exercises.iter().map(|e| e.exercise.as_str()).collect();
        assert_eq!(
            names,
            ["ex1", "ex2", "zzz"],
            "ex3 had nobody, so is left out"
        );
    }

    #[test]
    fn quiet_students_are_barely_there_or_never_typed() {
        let mut events = work("typist", "ex1", 0, 20, 2);
        events.extend(work("watcher", "ex1", 0, 20, 0));
        events.extend(work("blip", "ex1", 0, 2, 0));
        let r = run(events, &[], &[info("ex1")]);
        let quiet: Vec<_> = r
            .quiet
            .iter()
            .map(|q| (q.student.as_str(), q.why))
            .collect();
        assert_eq!(
            quiet,
            [("blip", "barely there"), ("watcher", "never typed")]
        );
    }

    #[test]
    fn a_class_whose_editors_report_no_edits_is_not_all_never_typed() {
        let events = [work("a", "ex1", 0, 20, 0), work("b", "ex1", 0, 20, 0)].concat();
        let r = run(events, &[], &[info("ex1")]);
        assert!(r.quiet.is_empty());
    }

    #[test]
    fn the_timeline_counts_distinct_students_per_five_minutes() {
        let mut events = work("a", "ex1", 0, 12, 1);
        events.extend(work("b", "ex1", 5, 7, 1));
        let r = run(events, &[], &[info("ex1")]);
        let active: Vec<_> = r.timeline.iter().map(|b| b.active).collect();
        assert_eq!(active, [1, 2, 2], "0-5: a; 5-10: a and b; 10-15: a and b");
    }

    #[test]
    fn messages_sent_during_the_lesson_are_listed_with_who_they_were_for() {
        let events = work("a", "ex1", 0, 10, 1);
        let sent = [
            Sent {
                at: 3 * MIN,
                text: "line 4".into(),
                to: Some("a".into()),
            },
            Sent {
                at: 6 * MIN,
                text: "five minutes".into(),
                to: None,
            },
        ];
        let r = build(span(&events), &events, &[], &sent, &[info("ex1")]);
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].to.as_deref(), Some("a"));
        assert_eq!(r.messages[1].to, None);
    }

    #[test]
    fn no_activity_is_an_empty_recap_not_a_panic() {
        let r = build(Span { from: 0, to: 0 }, &[], &[], &[], &[info("ex1")]);
        assert_eq!((r.students, r.median_active_seconds), (0, 0));
        assert!(r.exercises.is_empty() && r.follow_up.is_empty());
    }
}
