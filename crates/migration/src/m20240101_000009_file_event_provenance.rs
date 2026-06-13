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
                ALTER TABLE file_events ADD COLUMN IF NOT EXISTS student_source TEXT;
                ALTER TABLE file_events ADD COLUMN IF NOT EXISTS repo TEXT;
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
                ALTER TABLE file_events DROP COLUMN IF EXISTS repo;
                ALTER TABLE file_events DROP COLUMN IF EXISTS student_source;
                "#,
            )
            .await?;
        Ok(())
    }
}
