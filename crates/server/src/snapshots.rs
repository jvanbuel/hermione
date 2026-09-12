//! Live view of the file a student has on screen.
//!
//! File *activity* (which file, which exercise, how long) is append-only
//! history in Postgres. A file *snapshot* — the buffer's text, the cursor, and
//! the diff against the student's last commit — is a different thing: it is
//! what someone is looking at right now, useful for the thirty seconds a
//! teacher spends helping them and worthless afterwards. So it is never
//! persisted. Snapshots live in memory, expire in minutes, and are only
//! produced when a teacher actually asks:
//!
//! ```text
//! teacher opens the file pane
//!   → GET /api/students/file          (teacher-authenticated)
//!   → control frame over the student's existing message socket
//!   → extension reads the buffer + diffs it against HEAD
//!   → POST /api/file-snapshots        (enrollment token + verified identity)
//!   → cached here, returned on the teacher's next poll
//! ```
//!
//! Nothing is sent while nobody is watching, and a student whose config turns
//! content sharing off answers with a refusal rather than silence, so the
//! dashboard can say so plainly instead of spinning.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::auth::AuthCtx;
use crate::http::{resolve_course, CourseCtx, VerifiedStudent};
use crate::state::{AppState, ControlOut};

/// A snapshot older than this is dropped: a stale buffer is worse than none,
/// and it caps how long a student's text can sit in the server's memory.
const SNAPSHOT_TTL: Duration = Duration::from_secs(180);

/// Don't ask a student's editor more often than this, however many teachers
/// are watching them.
const REQUEST_INTERVAL: Duration = Duration::from_millis(750);

/// Hard cap on the buffer text the server will hold, independent of what the
/// extension already truncates to.
const MAX_CONTENT_BYTES: usize = 256 * 1024;

/// Hard cap on diff lines held per snapshot.
const MAX_DIFF_LINES: usize = 2000;

/// Students tracked per process before the store is pruned of expired entries.
const PRUNE_THRESHOLD: usize = 256;

/// One hunk of a unified diff, as produced by the extension.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub old_start: i32,
    pub old_lines: i32,
    pub new_start: i32,
    pub new_lines: i32,
    /// Lines prefixed the unified-diff way: ' ' context, '-' removed, '+' added.
    pub lines: Vec<String>,
    /// Spans for this hunk's removed lines, in order, added server-side.
    ///
    /// Context and added lines are already in the buffer, so the page reads
    /// their spans straight out of `FileSnapshot::highlight` by line number.
    /// Removed lines exist only in the student's last commit, which we never
    /// see, so they are the one side that has to be highlighted separately.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub removed_highlight: Option<Vec<Vec<crate::highlight::Token>>>,
}

/// The student's working changes to the file, against their last commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub added: i32,
    pub removed: i32,
    pub hunks: Vec<Hunk>,
    /// True when hunks were dropped to stay under the line cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// What one student has on screen at one moment.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSnapshot {
    pub student: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exercise: Option<String>,
    /// 1-based cursor line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i32>,
    /// 1-based cursor column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i32>,
    /// The buffer has unsaved changes (so it differs from the file on disk).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    /// The buffer's text. Absent when the student has sharing off, or has no
    /// file open at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// True when `content` was cut short at the size cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// What the diff is against: `head`, `untracked` (no committed version), or
    /// `none` (not a git working tree, or git was unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<Diff>,
    /// Set when the student's configuration forbids sharing file contents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declined: Option<bool>,
    pub at_unix_ms: i64,
    /// `content` split into one list of classed spans per line. Filled in here
    /// on arrival, never accepted from a client: highlighting is the server's
    /// reading of the buffer, and `content` stays the thing of record.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Vec<Vec<crate::highlight::Token>>>,
}

impl FileSnapshot {
    /// Trims a snapshot to the server's own caps, whatever the client sent.
    fn clamp(mut self) -> Self {
        if let Some(content) = self.content.take() {
            if content.len() > MAX_CONTENT_BYTES {
                // Cut on a char boundary so the result stays valid UTF-8.
                let mut end = MAX_CONTENT_BYTES;
                while end > 0 && !content.is_char_boundary(end) {
                    end -= 1;
                }
                self.content = Some(content[..end].to_string());
                self.truncated = Some(true);
            } else {
                self.content = Some(content);
            }
        }
        if let Some(diff) = self.diff.as_mut() {
            let mut budget = MAX_DIFF_LINES;
            let before = diff.hunks.len();
            diff.hunks.retain(|h| {
                let n = h.lines.len();
                if n <= budget {
                    budget -= n;
                    true
                } else {
                    budget = 0;
                    false
                }
            });
            if diff.hunks.len() < before {
                diff.truncated = Some(true);
            }
        }
        self
    }
}

struct Entry {
    snapshot: Option<Arc<FileSnapshot>>,
    stored_at: Instant,
    requested_at: Option<Instant>,
}

/// The in-memory cache of the latest snapshot per student.
#[derive(Clone, Default)]
pub struct SnapshotStore {
    entries: Arc<RwLock<HashMap<(Uuid, String), Entry>>>,
}

impl SnapshotStore {
    async fn put(&self, course_id: Uuid, snapshot: FileSnapshot) {
        let key = (course_id, snapshot.student.clone());
        let mut map = self.entries.write().await;
        if map.len() > PRUNE_THRESHOLD {
            map.retain(|_, e| e.stored_at.elapsed() < SNAPSHOT_TTL);
        }
        let requested_at = map.get(&key).and_then(|e| e.requested_at);
        map.insert(
            key,
            Entry {
                snapshot: Some(Arc::new(snapshot)),
                stored_at: Instant::now(),
                requested_at,
            },
        );
    }

    /// The student's latest snapshot, if one arrived recently enough to trust.
    async fn get(&self, course_id: Uuid, student: &str) -> Option<Arc<FileSnapshot>> {
        let map = self.entries.read().await;
        let entry = map.get(&(course_id, student.to_string()))?;
        (entry.stored_at.elapsed() < SNAPSHOT_TTL).then(|| entry.snapshot.clone())?
    }

    /// Records that a request is about to go out, and says whether enough time
    /// has passed to actually send one.
    async fn should_request(&self, course_id: Uuid, student: &str) -> bool {
        let mut map = self.entries.write().await;
        let entry = map
            .entry((course_id, student.to_string()))
            .or_insert_with(|| Entry {
                snapshot: None,
                stored_at: Instant::now(),
                requested_at: None,
            });
        let due = entry
            .requested_at
            .is_none_or(|at| at.elapsed() >= REQUEST_INTERVAL);
        if due {
            entry.requested_at = Some(Instant::now());
        }
        due
    }
}

/// The student's editor posts the file it has on screen. Gated exactly like
/// file events: enrollment token for the course, verified identity (when the
/// deployment enforces one) for who it is.
pub async fn ingest(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Json(mut snapshot): Json<FileSnapshot>,
) -> impl IntoResponse {
    if let Some(student) = verified {
        snapshot.student = student;
    }
    if snapshot.student.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing student").into_response();
    }

    // Highlight after clamping, so the spans describe the text we actually
    // kept, and once here rather than once per teacher per poll.
    let mut snapshot = snapshot.clamp();
    let language = snapshot.language.clone();
    let path = snapshot
        .relative_path
        .clone()
        .or_else(|| snapshot.path.clone());
    if let Some(content) = snapshot.content.as_deref() {
        snapshot.highlight =
            crate::highlight::highlight(content, language.as_deref(), path.as_deref());
    }
    // Only worth highlighting the removed side when the rest of the diff has
    // spans to sit next to; a half-coloured diff is worse than a plain one.
    if snapshot.highlight.is_some() {
        if let Some(diff) = snapshot.diff.as_mut() {
            highlight_removed(diff, language.as_deref(), path.as_deref());
        }
    }

    state.snapshots.put(course_id, snapshot).await;
    StatusCode::OK.into_response()
}

/// Fills in each hunk's `removed_highlight`.
///
/// The removed lines are parsed together with the hunk's context lines — the
/// old side of the hunk, in order — rather than on their own, so the parser
/// sees whatever surroundings the hunk itself carries. Only the rows belonging
/// to removed lines are kept; the rest of the diff reads its spans from the
/// buffer.
fn highlight_removed(diff: &mut Diff, language: Option<&str>, path: Option<&str>) {
    for hunk in diff.hunks.iter_mut() {
        let mut old_side = Vec::new();
        let mut removed = Vec::new();
        for line in &hunk.lines {
            if line.starts_with('+') {
                continue;
            }
            // Every diff line carries a one-byte ASCII sign, so this is always
            // a char boundary.
            old_side.push(line.get(1..).unwrap_or_default().to_string());
            removed.push(line.starts_with('-'));
        }
        if !removed.iter().any(|r| *r) {
            continue;
        }
        let Some(spans) = crate::highlight::highlight_lines(&old_side, language, path) else {
            continue;
        };
        hunk.removed_highlight = Some(
            spans
                .into_iter()
                .zip(&removed)
                .filter(|(_, keep)| **keep)
                .map(|(span, _)| span)
                .collect(),
        );
    }
}

#[derive(Deserialize)]
pub struct StudentFileQuery {
    student: String,
    course: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StudentFileResponse {
    student: String,
    /// The cached snapshot, or null until the editor answers.
    snapshot: Option<Arc<FileSnapshot>>,
    /// True when the student has an editor connected that we could ask.
    connected: bool,
    /// How old the snapshot is, so the dashboard can show staleness honestly.
    #[serde(skip_serializing_if = "Option::is_none")]
    age_ms: Option<i64>,
}

/// What a student has on screen right now. Each call also nudges their editor
/// for a fresh snapshot, so a teacher polling this endpoint gets a live view
/// and a student nobody is watching is never asked for anything.
pub async fn student_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<StudentFileQuery>,
) -> Response {
    let course_id = match resolve_course(&state, ctx, q.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if q.student.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing student").into_response();
    }

    let connected = if state.snapshots.should_request(course_id, &q.student).await {
        state
            .ctrl_hub
            .publish(
                course_id,
                &q.student,
                ControlOut {
                    kind: "snapshot-request".to_string(),
                },
            )
            .await
    } else {
        // Too soon to ask again; report reachability from whether a snapshot
        // has landed recently rather than re-probing the socket.
        state.snapshots.get(course_id, &q.student).await.is_some()
    };

    let snapshot = state.snapshots.get(course_id, &q.student).await;
    let age_ms = snapshot
        .as_ref()
        .map(|s| (chrono::Utc::now().timestamp_millis() - s.at_unix_ms).max(0));

    Json(StudentFileResponse {
        student: q.student,
        snapshot,
        connected,
        age_ms,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(content: &str) -> FileSnapshot {
        FileSnapshot {
            student: "alice".to_string(),
            path: None,
            relative_path: Some("ex1/main.py".to_string()),
            language: None,
            exercise: None,
            line: Some(3),
            column: Some(1),
            dirty: None,
            content: Some(content.to_string()),
            truncated: None,
            base: Some("head".to_string()),
            diff: None,
            declined: None,
            at_unix_ms: 0,
            highlight: None,
        }
    }

    #[test]
    fn clamps_oversized_content_on_a_char_boundary() {
        // A multi-byte char straddling the cap must not be cut in half.
        let big = "é".repeat(MAX_CONTENT_BYTES);
        let clamped = snapshot(&big).clamp();
        let content = clamped.content.unwrap();
        assert!(content.len() <= MAX_CONTENT_BYTES);
        assert_eq!(clamped.truncated, Some(true));
        assert!(content.chars().all(|c| c == 'é'));
    }

    #[test]
    fn keeps_content_that_fits() {
        let clamped = snapshot("print('hi')").clamp();
        assert_eq!(clamped.content.as_deref(), Some("print('hi')"));
        assert_eq!(clamped.truncated, None);
    }

    #[test]
    fn drops_hunks_past_the_line_cap() {
        let hunk = |n: usize| Hunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: vec!["+x".to_string(); n],
            removed_highlight: None,
        };
        let mut s = snapshot("x");
        s.diff = Some(Diff {
            added: 1,
            removed: 0,
            hunks: vec![hunk(MAX_DIFF_LINES), hunk(1)],
            truncated: None,
        });
        let diff = s.clamp().diff.unwrap();
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.truncated, Some(true));
    }

    #[test]
    fn highlights_only_the_removed_side_of_a_hunk() {
        let mut diff = Diff {
            added: 1,
            removed: 2,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 4,
                new_start: 1,
                new_lines: 3,
                lines: vec![
                    " def f():".to_string(),
                    "-    return 1".to_string(),
                    "-    # gone".to_string(),
                    "+    return 2".to_string(),
                    " ".to_string(),
                ],
                removed_highlight: None,
            }],
            truncated: None,
        };
        highlight_removed(&mut diff, Some("python"), None);

        let spans = diff.hunks[0].removed_highlight.as_ref().expect("spans");
        // One row per removed line, and nothing for context or added lines.
        assert_eq!(spans.len(), 2);
        let text = |row: &Vec<crate::highlight::Token>| -> String {
            row.iter().map(|t| t.1.as_str()).collect()
        };
        assert_eq!(text(&spans[0]), "    return 1");
        assert_eq!(text(&spans[1]), "    # gone");
        assert!(
            spans[1].iter().any(|t| t.0 == "c"),
            "a removed comment is still a comment: {:?}",
            spans[1]
        );
    }

    #[test]
    fn a_hunk_with_nothing_removed_gets_no_spans() {
        let mut diff = Diff {
            added: 1,
            removed: 0,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                lines: vec![" x = 1".to_string(), "+y = 2".to_string()],
                removed_highlight: None,
            }],
            truncated: None,
        };
        highlight_removed(&mut diff, Some("python"), None);
        assert!(diff.hunks[0].removed_highlight.is_none());
    }

    #[tokio::test]
    async fn expired_snapshots_are_not_served() {
        let store = SnapshotStore::default();
        let course = Uuid::new_v4();
        store.put(course, snapshot("x")).await;
        assert!(store.get(course, "alice").await.is_some());

        // Age the entry past the TTL without sleeping for it.
        {
            let mut map = store.entries.write().await;
            let entry = map.get_mut(&(course, "alice".to_string())).unwrap();
            entry.stored_at = Instant::now() - SNAPSHOT_TTL - Duration::from_secs(1);
        }
        assert!(store.get(course, "alice").await.is_none());
    }

    #[tokio::test]
    async fn requests_are_throttled_per_student() {
        let store = SnapshotStore::default();
        let course = Uuid::new_v4();
        assert!(store.should_request(course, "alice").await);
        assert!(!store.should_request(course, "alice").await);
        // A different student is throttled independently.
        assert!(store.should_request(course, "bob").await);
    }

    #[tokio::test]
    async fn a_new_snapshot_keeps_the_request_throttle() {
        let store = SnapshotStore::default();
        let course = Uuid::new_v4();
        assert!(store.should_request(course, "alice").await);
        store.put(course, snapshot("x")).await;
        assert!(!store.should_request(course, "alice").await);
    }
}
