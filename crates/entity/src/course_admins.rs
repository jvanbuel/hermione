//! Membership join table: which admins can access which courses.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "course_admins")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub admin_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub course_id: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
