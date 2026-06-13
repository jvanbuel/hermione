//! One turn in an assistant conversation — a student question or an assistant
//! reply. Persisted for chat history and teacher visibility.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "assistant_messages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub conversation_id: Uuid,
    /// "student" or "assistant".
    pub role: String,
    pub body: String,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
