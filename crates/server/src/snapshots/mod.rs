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
//!
//! The pieces, from the outside in: [`report`] is what an editor sends;
//! [`model`] is what we keep and show; [`diff`] turns a diff into rows;
//! [`store`] holds the latest per student.

mod diff;
mod model;
mod report;
mod store;

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::auth::AuthCtx;
use crate::http::{resolve_course, CourseCtx, VerifiedStudent};
use crate::state::{AppState, Control};
use crate::student::{Slot, Student};

pub use store::SnapshotStore;

use model::Snapshot;
use report::Report;
use store::Latest;

/// The student's editor posts the file it has on screen. Gated exactly like
/// file events: enrollment token for the course, verified identity (when the
/// deployment enforces one) for who it is.
pub async fn ingest(
    State(state): State<AppState>,
    Extension(CourseCtx(course)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Json(report): Json<Report>,
) -> Response {
    // A verified identity overrides the self-asserted name. It must not fall
    // back to it when the identity is unusable: that would let an editor pick
    // its own name on exactly the deployments that verify them.
    let student = match verified.map(Student::try_from).transpose() {
        Ok(verified) => verified.unwrap_or(report.student),
        Err(e) => return (StatusCode::UNAUTHORIZED, e.to_string()).into_response(),
    };

    // Turning a report into a snapshot parses the whole buffer: tens to
    // hundreds of milliseconds of solid CPU, and a watched student sends one
    // every few hundred ms. On a Tokio worker that stalls every other request
    // sharing the thread, so it goes where argon2 goes (see tenancy.rs).
    let state_of = report.state;
    let snapshot = match tokio::task::spawn_blocking(move || Snapshot::from(state_of)).await {
        Ok(snapshot) => snapshot,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    state.snapshots.insert(Slot { course, student }, snapshot);
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
pub struct FileQuery {
    student: Student,
    course: Option<String>,
}

/// What a teacher polls for.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct View<'a> {
    student: &'a Student,
    /// Whether an editor is connected that could answer.
    connected: bool,
    /// The latest snapshot, or `None` until the editor answers.
    latest: Option<LatestView<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LatestView<'a> {
    /// Milliseconds since the server received it.
    age_ms: u64,
    rev: u64,
    snapshot: &'a Snapshot,
}

impl<'a> From<&'a Latest> for LatestView<'a> {
    fn from(latest: &'a Latest) -> Self {
        Self {
            age_ms: latest.age.as_millis().try_into().unwrap_or(u64::MAX),
            rev: latest.rev,
            snapshot: &latest.snapshot,
        }
    }
}

/// What a student has on screen right now. Each call also nudges their editor
/// for a fresh snapshot, so a teacher polling this endpoint gets a live view
/// and a student nobody is watching is never asked for anything.
pub async fn student_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<FileQuery>,
) -> Response {
    let course = match resolve_course(&state, ctx, q.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let slot = Slot {
        course,
        student: q.student,
    };

    if state.snapshots.claim_ask(&slot) {
        state
            .ctrl_hub
            .publish(&slot, Control::SnapshotRequest)
            .await;
    }

    let latest = state.snapshots.latest(&slot);
    Json(View {
        student: &slot.student,
        // Whether an editor is there to answer is the hub's to say, not
        // something to infer from the side effects of a poll that may have been
        // throttled.
        connected: state.ctrl_hub.is_listening(&slot).await,
        latest: latest.as_ref().map(LatestView::from),
    })
    .into_response()
}
