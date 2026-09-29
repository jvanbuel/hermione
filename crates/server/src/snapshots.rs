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
use crate::highlight::Token;
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

/// One hunk of a unified diff. It arrives from the extension as `lines` and
/// leaves for the dashboard as `rows`: the server does the work of finding each
/// line's spans once, so the page just draws what it is given.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub old_start: i32,
    pub old_lines: i32,
    pub new_start: i32,
    pub new_lines: i32,
    /// As sent, prefixed the unified-diff way: ' ' context, '-' removed, '+'
    /// added. Consumed into `rows` on arrival and never sent on.
    #[serde(skip_serializing)]
    pub lines: Vec<String>,
    /// The same lines, ready to draw. Filled in here, never accepted from a client.
    #[serde(skip_deserializing)]
    pub rows: Vec<Row>,
}

/// One line of a hunk. The text is the concatenation of `spans`, exactly as
/// with `FileSnapshot::highlight`; an unhighlighted line is a single classless
/// span.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    /// ' ' context, '-' removed, '+' added.
    pub sign: char,
    /// Line number in the committed file. Absent on an added line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<i32>,
    /// Line number in the buffer. Absent on a removed line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<i32>,
    pub spans: Vec<crate::highlight::Token>,
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
    fn clamp(&mut self) {
        if let Some(content) = self.content.as_mut() {
            if content.len() > MAX_CONTENT_BYTES {
                // Cut on a char boundary so the result stays valid UTF-8.
                // Byte 0 always is one, so this terminates.
                let mut end = MAX_CONTENT_BYTES;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                content.truncate(end);
                self.truncated = Some(true);
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
    }
}

/// A student's snapshot slot: which course, and which student in it.
type Key = (Uuid, String);

struct Entry {
    snapshot: Option<Arc<FileSnapshot>>,
    /// When this entry was last written — by a snapshot or by a request. Both
    /// keep it alive through pruning.
    touched_at: Instant,
    requested_at: Option<Instant>,
}

/// The in-memory cache of the latest snapshot per student.
#[derive(Clone, Default)]
pub struct SnapshotStore {
    entries: Arc<RwLock<HashMap<Key, Entry>>>,
}

impl SnapshotStore {
    async fn put(&self, key: Key, snapshot: FileSnapshot) {
        let mut map = self.entries.write().await;
        if map.len() > PRUNE_THRESHOLD {
            map.retain(|_, e| e.touched_at.elapsed() < SNAPSHOT_TTL);
        }
        let requested_at = map.get(&key).and_then(|e| e.requested_at);
        map.insert(
            key,
            Entry {
                snapshot: Some(Arc::new(snapshot)),
                touched_at: Instant::now(),
                requested_at,
            },
        );
    }

    /// The student's latest snapshot, if one arrived recently enough to trust.
    async fn get(&self, key: &Key) -> Option<Arc<FileSnapshot>> {
        self.entries
            .read()
            .await
            .get(key)
            .filter(|e| e.touched_at.elapsed() < SNAPSHOT_TTL)?
            .snapshot
            .clone()
    }

    /// Records that a request is about to go out, and says whether enough time
    /// has passed to actually send one.
    async fn should_request(&self, key: &Key) -> bool {
        let mut map = self.entries.write().await;
        let entry = map.entry(key.clone()).or_insert_with(|| Entry {
            snapshot: None,
            touched_at: Instant::now(),
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

    snapshot.clamp();
    let key = (course_id, snapshot.student.clone());

    // Parsing a buffer is tens to hundreds of milliseconds of solid CPU, and a
    // watched student sends one of these every few hundred ms. On a Tokio
    // worker that stalls every other request sharing the thread, so it goes
    // where argon2 goes (see tenancy.rs).
    let snapshot = match tokio::task::spawn_blocking(move || {
        highlight_snapshot(&mut snapshot);
        snapshot
    })
    .await
    {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    state.snapshots.put(key, snapshot).await;
    StatusCode::OK.into_response()
}

/// Adds the spans the dashboard renders from. Runs after clamping, so they
/// describe the text we actually kept, and once on arrival rather than once
/// per teacher per poll.
fn highlight_snapshot(snapshot: &mut FileSnapshot) {
    let language = snapshot.language.as_deref();
    let path = snapshot
        .relative_path
        .as_deref()
        .or(snapshot.path.as_deref());
    let highlight = snapshot
        .content
        .as_deref()
        .and_then(|c| crate::highlight::highlight(c, language, path));

    // Rows are built even when there is nothing to highlight: the page draws
    // rows, not raw lines, so an unhighlighted diff is rows of plain spans.
    if let Some(diff) = snapshot.diff.as_mut() {
        for hunk in diff.hunks.iter_mut() {
            build_rows(hunk, highlight.as_deref(), language, path);
        }
    }
    snapshot.highlight = highlight;
}

/// Turns a hunk's `lines` into `rows`, each with the spans it will be drawn in.
///
/// Context and added lines are lines of the buffer, so their spans are read out
/// of `buffer` by line number and get the whole file's context for free.
/// Removed lines exist only in the student's last commit, which we never see,
/// so they are the one side highlighted separately: the hunk's old side —
/// context and removed lines, in order — is parsed as one fragment, so the
/// parser sees whatever surroundings the hunk carries.
///
/// Spans are only used for a line if they rebuild that line's text exactly. A
/// buffer clamped at the size cap has fewer lines than its diff refers to, and
/// drawing text a student never typed is the one failure worth guarding
/// against. Doing the check here means it happens once per snapshot, not once
/// per row per poll in the browser.
fn build_rows(
    hunk: &mut Hunk,
    buffer: Option<&[Vec<Token>]>,
    language: Option<&str>,
    path: Option<&str>,
) {
    let lines = std::mem::take(&mut hunk.lines);
    // Every diff line carries a one-byte ASCII sign, so slicing from 1 is
    // always a char boundary.
    let text_of = |line: &str| line.get(1..).unwrap_or_default().to_string();

    let old_side: Vec<String> = lines
        .iter()
        .filter(|l| !l.starts_with('+'))
        .map(|l| text_of(l))
        .collect();
    let old_spans = (buffer.is_some() && lines.iter().any(|l| l.starts_with('-')))
        .then(|| {
            let text: Vec<&str> = old_side.iter().map(String::as_str).collect();
            crate::highlight::highlight_lines(&text, language, path)
        })
        .flatten();

    let (mut old, mut new) = (hunk.old_start, hunk.new_start);
    let mut old_index = 0;
    hunk.rows = lines
        .iter()
        .map(|line| {
            let sign = line.chars().next().unwrap_or(' ');
            let text = text_of(line);
            let (old_no, new_no, spans) = match sign {
                '+' => {
                    let spans = buffer_spans(buffer, new);
                    new += 1;
                    (None, Some(new - 1), spans)
                }
                '-' => {
                    let spans = old_spans.as_ref().and_then(|s| s.get(old_index));
                    old += 1;
                    old_index += 1;
                    (Some(old - 1), None, spans)
                }
                _ => {
                    let spans = buffer_spans(buffer, new);
                    old += 1;
                    new += 1;
                    old_index += 1;
                    (Some(old - 1), Some(new - 1), spans)
                }
            };
            Row {
                sign,
                old: old_no,
                new: new_no,
                spans: match spans {
                    Some(spans) if rebuilds(spans, &text) => spans.clone(),
                    _ => vec![Token("", text)],
                },
            }
        })
        .collect();
}

/// The buffer's spans for a 1-based line number, if there are any.
fn buffer_spans(buffer: Option<&[Vec<Token>]>, line: i32) -> Option<&Vec<Token>> {
    buffer?.get(usize::try_from(line.checked_sub(1)?).ok()?)
}

/// Whether `spans` concatenate back to exactly `text`.
fn rebuilds(spans: &[Token], text: &str) -> bool {
    let mut rest = text;
    for span in spans {
        match rest.strip_prefix(span.1.as_str()) {
            Some(after) => rest = after,
            None => return false,
        }
    }
    rest.is_empty()
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

    let key = (course_id, q.student.clone());
    if state.snapshots.should_request(&key).await {
        let frame = ControlOut {
            kind: "snapshot-request",
        };
        state.ctrl_hub.publish(&key, frame).await;
    }

    // Whether an editor is there to answer is the hub's to say, not something
    // to infer from the side effects of a poll that may have been throttled.
    let connected = state.ctrl_hub.listeners(&key).await > 0;
    let snapshot = state.snapshots.get(&key).await;
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
        let mut clamped = snapshot(&big);
        clamped.clamp();
        let content = clamped.content.clone().unwrap();
        assert!(content.len() <= MAX_CONTENT_BYTES);
        assert_eq!(clamped.truncated, Some(true));
        assert!(content.chars().all(|c| c == 'é'));
    }

    #[test]
    fn keeps_content_that_fits() {
        let mut clamped = snapshot("print('hi')");
        clamped.clamp();
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
            rows: vec![],
        };
        let mut s = snapshot("x");
        s.diff = Some(Diff {
            added: 1,
            removed: 0,
            hunks: vec![hunk(MAX_DIFF_LINES), hunk(1)],
            truncated: None,
        });
        s.clamp();
        let diff = s.diff.unwrap();
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.truncated, Some(true));
    }

    /// A hunk over a small Python file, as the extension would send it.
    fn hunk(lines: &[&str]) -> Hunk {
        Hunk {
            old_start: 1,
            old_lines: 4,
            new_start: 1,
            new_lines: 3,
            lines: lines.iter().map(|l| l.to_string()).collect(),
            rows: vec![],
        }
    }

    fn text(row: &Row) -> String {
        row.spans.iter().map(|t| t.1.as_str()).collect()
    }

    #[test]
    fn rows_carry_signs_numbers_and_their_own_spans() {
        // The buffer is what the student sees now: the removed lines are not in it.
        let buffer = "def f():\n    return 2\n\n";
        let spans = crate::highlight::highlight(buffer, Some("python"), None).unwrap();

        let mut h = hunk(&[
            " def f():",
            "-    return 1",
            "-    # gone",
            "+    return 2",
            " ",
        ]);
        build_rows(&mut h, Some(&spans), Some("python"), None);

        assert!(h.lines.is_empty(), "consumed into rows");
        let signs: String = h.rows.iter().map(|r| r.sign).collect();
        assert_eq!(signs, " --+ ");
        // Old and new numbers advance independently, and are absent on the side
        // a row isn't on.
        let nums: Vec<_> = h.rows.iter().map(|r| (r.old, r.new)).collect();
        assert_eq!(
            nums,
            [
                (Some(1), Some(1)),
                (Some(2), None),
                (Some(3), None),
                (None, Some(2)),
                (Some(4), Some(3)),
            ]
        );
        // Every row draws exactly the text it was sent.
        let drawn: Vec<_> = h.rows.iter().map(text).collect();
        assert_eq!(
            drawn,
            ["def f():", "    return 1", "    # gone", "    return 2", ""]
        );
        // A removed comment — which exists in no buffer — is still a comment.
        assert!(
            h.rows[2].spans.iter().any(|t| t.0 == "c"),
            "{:?}",
            h.rows[2]
        );
        // An added line takes the buffer's spans, keyword and all.
        assert!(
            h.rows[3].spans.iter().any(|t| t.0 == "k"),
            "{:?}",
            h.rows[3]
        );
    }

    #[test]
    fn rows_are_plain_when_there_is_nothing_to_highlight() {
        let mut h = hunk(&[" x = 1", "-y = 2", "+y = 3"]);
        build_rows(&mut h, None, Some("python"), None);
        for row in &h.rows {
            assert_eq!(row.spans.len(), 1, "{row:?}");
            assert_eq!(row.spans[0].0, "");
        }
        assert_eq!(text(&h.rows[1]), "y = 2");
    }

    #[test]
    fn spans_that_do_not_belong_to_a_line_are_not_used() {
        // A buffer clamped short: line 2 of the diff points at a line that is
        // not in the spans, and line 1's spans are for different text entirely.
        let spans = crate::highlight::highlight("z = 9\n", Some("python"), None).unwrap();
        let mut h = hunk(&[" a = 1", "+b = 2"]);
        build_rows(&mut h, Some(&spans), Some("python"), None);
        assert_eq!(text(&h.rows[0]), "a = 1");
        assert_eq!(text(&h.rows[1]), "b = 2");
        for row in &h.rows {
            assert_eq!(row.spans.len(), 1, "fell back to plain: {row:?}");
        }
    }

    #[test]
    fn rows_serialize_without_the_raw_lines() {
        let mut h = hunk(&[" a = 1", "+b = 2"]);
        build_rows(&mut h, None, None, None);
        let json = serde_json::to_value(&h).unwrap();
        assert!(json.get("lines").is_none(), "raw lines are not sent on");
        assert_eq!(json["rows"][1]["sign"], "+");
        assert_eq!(json["rows"][1]["new"], 2);
        assert!(json["rows"][1].get("old").is_none());
    }

    #[tokio::test]
    async fn expired_snapshots_are_not_served() {
        let store = SnapshotStore::default();
        let alice = (Uuid::new_v4(), "alice".to_string());
        store.put(alice.clone(), snapshot("x")).await;
        assert!(store.get(&alice).await.is_some());

        // Age the entry past the TTL without sleeping for it.
        {
            let mut map = store.entries.write().await;
            let entry = map.get_mut(&alice).unwrap();
            entry.touched_at = Instant::now() - SNAPSHOT_TTL - Duration::from_secs(1);
        }
        assert!(store.get(&alice).await.is_none());
    }

    #[tokio::test]
    async fn requests_are_throttled_per_student() {
        let store = SnapshotStore::default();
        let course = Uuid::new_v4();
        let alice = (course, "alice".to_string());
        assert!(store.should_request(&alice).await);
        assert!(!store.should_request(&alice).await);
        // A different student is throttled independently.
        assert!(store.should_request(&(course, "bob".to_string())).await);
    }

    #[tokio::test]
    async fn a_new_snapshot_keeps_the_request_throttle() {
        let store = SnapshotStore::default();
        let alice = (Uuid::new_v4(), "alice".to_string());
        assert!(store.should_request(&alice).await);
        store.put(alice.clone(), snapshot("x")).await;
        assert!(!store.should_request(&alice).await);
    }
}
