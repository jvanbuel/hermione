//! Per-course AI teaching-assistant configuration. A row exists only once a
//! teacher has opened the assistant settings for the course; its absence means
//! the course has no assistant and behaves exactly as before.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "course_assistants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub course_id: Uuid,
    pub enabled: bool,
    pub model: String,
    pub system_prompt: String,
    /// JSON array (text) of skill refs: `[{type, skillId, version?}]`.
    pub skills: String,
    /// JSON array (text) of MCP servers: `[{name, url}]`.
    pub mcp_servers: String,
    /// The synced Anthropic Managed Agents resources (set once configured).
    pub agent_id: Option<String>,
    pub agent_version: Option<String>,
    pub environment_id: Option<String>,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
