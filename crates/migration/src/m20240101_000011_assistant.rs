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
                -- Per-course AI teaching-assistant configuration. A course has a
                -- row only once a teacher opens the settings; absence ⇒ no
                -- assistant, and the course works exactly as before.
                CREATE TABLE IF NOT EXISTS course_assistants (
                    course_id      UUID PRIMARY KEY REFERENCES courses(id) ON DELETE CASCADE,
                    enabled        BOOLEAN     NOT NULL DEFAULT false,
                    model          TEXT        NOT NULL DEFAULT 'claude-opus-4-8',
                    system_prompt  TEXT        NOT NULL DEFAULT '',
                    -- JSON arrays (text) of skill refs and MCP servers; parsed in
                    -- the app so we don't depend on a JSONB SeaORM feature.
                    skills         TEXT        NOT NULL DEFAULT '[]',
                    mcp_servers    TEXT        NOT NULL DEFAULT '[]',
                    -- The synced Anthropic Managed Agents resources.
                    agent_id       TEXT,
                    agent_version  TEXT,
                    environment_id TEXT,
                    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
                );

                -- One assistant conversation per student per course, mapped to a
                -- Managed Agents session.
                CREATE TABLE IF NOT EXISTS assistant_conversations (
                    id          UUID PRIMARY KEY,
                    course_id   UUID        NOT NULL REFERENCES courses(id) ON DELETE CASCADE,
                    student     TEXT        NOT NULL,
                    session_id  TEXT,
                    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
                    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
                    UNIQUE (course_id, student)
                );

                -- The transcript: both the student's questions and the
                -- assistant's replies, for history and teacher visibility.
                CREATE TABLE IF NOT EXISTS assistant_messages (
                    id              BIGSERIAL PRIMARY KEY,
                    conversation_id UUID        NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
                    role            TEXT        NOT NULL,
                    body            TEXT        NOT NULL,
                    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
                );
                CREATE INDEX IF NOT EXISTS idx_assistant_messages_conversation
                    ON assistant_messages (conversation_id, id);
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
                DROP TABLE IF EXISTS assistant_messages;
                DROP TABLE IF EXISTS assistant_conversations;
                DROP TABLE IF EXISTS course_assistants;
                "#,
            )
            .await?;
        Ok(())
    }
}
