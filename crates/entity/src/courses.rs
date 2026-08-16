//! A course — the tenant boundary. All sessions and file activity belong to a
//! course, and admins are granted access per course.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "courses")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub slug: String,
    pub name: String,
    /// Secret presented by recorders/the extension to enroll into this course.
    #[sea_orm(unique)]
    pub enrollment_token: String,
    /// The git repository this course is linked to, if any (the assignment or
    /// template repo it was created from). Used for provenance and to derive a
    /// default slug/name.
    pub repo_url: Option<String>,
    /// When the course was archived (dropped from the active switcher). `None`
    /// while the course is active.
    pub archived_at: Option<DateTimeWithTimeZone>,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
