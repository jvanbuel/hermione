use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                -- Where a course keeps its reference solutions inside the linked
                -- repo: a branch/tag/commit and/or a folder. Both optional; a
                -- course with neither has no solutions to compare against.
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS solutions_ref TEXT;
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS solutions_dir TEXT;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE courses DROP COLUMN IF EXISTS solutions_ref;
                ALTER TABLE courses DROP COLUMN IF EXISTS solutions_dir;
                "#,
            )
            .await?;
        Ok(())
    }
}
