//! A single recorded terminal session.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub student: String,
    pub command: String,
    pub hostname: Option<String>,
    pub cols: i32,
    pub rows: i32,
    /// "active" while recording, "ended" once the process exits.
    pub status: String,
    pub started_at: DateTimeWithTimeZone,
    pub ended_at: Option<DateTimeWithTimeZone>,
    pub exit_code: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::terminal_events::Entity")]
    TerminalEvents,
}

impl Related<super::terminal_events::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TerminalEvents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
