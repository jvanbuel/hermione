//! An append-only slice of terminal activity belonging to a session.
//!
//! `data` holds the raw terminal bytes encoded as base64, so that arbitrary
//! (possibly non-UTF-8) byte streams can live safely in a TEXT column.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "terminal_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub session_id: Uuid,
    /// Monotonic per-session sequence number, preserving event order.
    pub seq: i64,
    /// Milliseconds since the session started.
    pub offset_ms: i64,
    /// "stdout" or "stdin".
    pub stream: String,
    /// Base64-encoded raw terminal bytes (verbatim, including ANSI escapes).
    pub data: String,
    /// ANSI-stripped, lossy-UTF8 plain text — readable and searchable for
    /// offline analysis. `None` only for legacy rows written before this column.
    pub text: Option<String>,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sessions::Entity",
        from = "Column::SessionId",
        to = "super::sessions::Column::Id"
    )]
    Session,
}

impl Related<super::sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Session.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
