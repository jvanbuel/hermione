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
                CREATE TABLE IF NOT EXISTS exercises (
                    id         UUID PRIMARY KEY,
                    course_id  UUID        NOT NULL REFERENCES courses(id) ON DELETE CASCADE,
                    slug       TEXT        NOT NULL,
                    title      TEXT        NOT NULL,
                    position   INTEGER     NOT NULL DEFAULT 0,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    UNIQUE (course_id, slug)
                );
                CREATE INDEX IF NOT EXISTS idx_exercises_course
                    ON exercises (course_id, position);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS exercises;")
            .await?;
        Ok(())
    }
}
