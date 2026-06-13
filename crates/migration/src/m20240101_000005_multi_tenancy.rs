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
                CREATE TABLE IF NOT EXISTS admins (
                    id            UUID PRIMARY KEY,
                    username      TEXT UNIQUE NOT NULL,
                    password_hash TEXT        NOT NULL,
                    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
                );

                CREATE TABLE IF NOT EXISTS courses (
                    id               UUID PRIMARY KEY,
                    slug             TEXT UNIQUE NOT NULL,
                    name             TEXT        NOT NULL,
                    enrollment_token TEXT UNIQUE NOT NULL,
                    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
                );

                CREATE TABLE IF NOT EXISTS course_admins (
                    admin_id  UUID NOT NULL REFERENCES admins(id)  ON DELETE CASCADE,
                    course_id UUID NOT NULL REFERENCES courses(id) ON DELETE CASCADE,
                    PRIMARY KEY (admin_id, course_id)
                );

                -- A default course backs open dev mode and adopts pre-tenancy data.
                INSERT INTO courses (id, slug, name, enrollment_token)
                VALUES ('00000000-0000-0000-0000-000000000001', 'default',
                        'Default course', 'default-token')
                ON CONFLICT DO NOTHING;

                ALTER TABLE sessions     ADD COLUMN IF NOT EXISTS course_id UUID REFERENCES courses(id);
                ALTER TABLE file_events  ADD COLUMN IF NOT EXISTS course_id UUID REFERENCES courses(id);

                UPDATE sessions    SET course_id = '00000000-0000-0000-0000-000000000001' WHERE course_id IS NULL;
                UPDATE file_events SET course_id = '00000000-0000-0000-0000-000000000001' WHERE course_id IS NULL;

                CREATE INDEX IF NOT EXISTS idx_sessions_course     ON sessions (course_id);
                CREATE INDEX IF NOT EXISTS idx_file_events_course  ON file_events (course_id, at);
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
                ALTER TABLE file_events DROP COLUMN IF EXISTS course_id;
                ALTER TABLE sessions    DROP COLUMN IF EXISTS course_id;
                DROP TABLE IF EXISTS course_admins;
                DROP TABLE IF EXISTS courses;
                DROP TABLE IF EXISTS admins;
                "#,
            )
            .await?;
        Ok(())
    }
}
