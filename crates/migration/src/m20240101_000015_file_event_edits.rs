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
                ALTER TABLE file_events ADD COLUMN IF NOT EXISTS edits INTEGER;
                ALTER TABLE file_events ADD COLUMN IF NOT EXISTS line INTEGER;
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
                ALTER TABLE file_events DROP COLUMN IF EXISTS line;
                ALTER TABLE file_events DROP COLUMN IF EXISTS edits;
                "#,
            )
            .await?;
        Ok(())
    }
}
