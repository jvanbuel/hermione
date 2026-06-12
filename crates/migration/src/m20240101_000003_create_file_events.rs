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
                CREATE TABLE IF NOT EXISTS file_events (
                    id            BIGSERIAL PRIMARY KEY,
                    student       TEXT        NOT NULL,
                    workspace     TEXT,
                    path          TEXT        NOT NULL,
                    relative_path TEXT,
                    language      TEXT,
                    exercise      TEXT,
                    kind          TEXT        NOT NULL,
                    at            TIMESTAMPTZ NOT NULL,
                    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
                );

                CREATE INDEX IF NOT EXISTS idx_file_events_student_at
                    ON file_events (student, at);

                CREATE INDEX IF NOT EXISTS idx_file_events_exercise
                    ON file_events (exercise);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS file_events;")
            .await?;
        Ok(())
    }
}
