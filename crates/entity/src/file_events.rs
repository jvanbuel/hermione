//! A point-in-time observation of which file a student has open/active in their
//! editor. A stream of these (focus changes, periodic heartbeats, and coalesced
//! edit bursts) lets us show live activity, compute time-on-task per file and
//! per exercise, and tell a student who is typing from one who is stuck.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "file_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Course (tenant) this activity belongs to.
    pub course_id: Option<Uuid>,
    pub student: String,
    /// Workspace/folder name the file belongs to.
    pub workspace: Option<String>,
    /// Absolute file path on the student's machine.
    pub path: String,
    /// Path relative to the workspace root, when available.
    pub relative_path: Option<String>,
    /// Editor language id (e.g. "rust", "python").
    pub language: Option<String>,
    /// Exercise this file maps to, if the extension could resolve one.
    pub exercise: Option<String>,
    /// How the student identity was derived ("github", "git-email", "config", …).
    pub student_source: Option<String>,
    /// The git repo the activity came from (owner/name), for provenance.
    pub repo: Option<String>,
    /// "focus" (became active), "heartbeat" (still active), "edit" (typed), or
    /// "close".
    pub kind: String,
    /// Document changes coalesced into this event. Only set on "edit" events;
    /// clients that predate edit reporting never send it.
    pub edits: Option<i32>,
    /// 1-based cursor line at the time of the event, when the file was on screen.
    pub line: Option<i32>,
    /// When the event happened on the client.
    pub at: DateTimeWithTimeZone,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
