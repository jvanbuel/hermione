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
                -- Who a message is for. NULL is the whole course (every message so
                -- far); a name is that one student. Sending to several students
                -- stores one row each, so a row is always addressed to exactly one
                -- audience and a reader's filter is a single comparison.
                ALTER TABLE messages ADD COLUMN IF NOT EXISTS student TEXT;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE messages DROP COLUMN IF EXISTS student;")
            .await?;
        Ok(())
    }
}
