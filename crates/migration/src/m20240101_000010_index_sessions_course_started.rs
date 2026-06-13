use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Serves the dashboard overview's recent-window scan over a course's
        // sessions, ordered by start time. The existing idx_sessions_course
        // (course_id only) can't satisfy the started_at range + ordering.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_sessions_course_started \
                 ON sessions (course_id, started_at);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_sessions_course_started;")
            .await?;
        Ok(())
    }
}
