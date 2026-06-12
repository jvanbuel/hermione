use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id          UUID PRIMARY KEY,
                student     TEXT        NOT NULL,
                command     TEXT        NOT NULL,
                hostname    TEXT,
                cols        INTEGER     NOT NULL DEFAULT 80,
                rows        INTEGER     NOT NULL DEFAULT 24,
                status      TEXT        NOT NULL DEFAULT 'active',
                started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
                ended_at    TIMESTAMPTZ,
                exit_code   INTEGER
            );

            CREATE TABLE IF NOT EXISTS terminal_events (
                id          BIGSERIAL PRIMARY KEY,
                session_id  UUID        NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq         BIGINT      NOT NULL,
                offset_ms   BIGINT      NOT NULL,
                stream      TEXT        NOT NULL,
                data        TEXT        NOT NULL,
                created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
            );

            CREATE INDEX IF NOT EXISTS idx_terminal_events_session_seq
                ON terminal_events (session_id, seq);

            CREATE INDEX IF NOT EXISTS idx_sessions_status
                ON sessions (status);
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            DROP TABLE IF EXISTS terminal_events;
            DROP TABLE IF EXISTS sessions;
            "#,
        )
        .await?;
        Ok(())
    }
}
