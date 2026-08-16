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
                -- A course's descriptive profile: free-text metadata beyond its
                -- structural identity. All optional (NULL ⇒ unset).
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS description TEXT;
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS term        TEXT;
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS institution TEXT;
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS level       TEXT;
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
                ALTER TABLE courses DROP COLUMN IF EXISTS description;
                ALTER TABLE courses DROP COLUMN IF EXISTS term;
                ALTER TABLE courses DROP COLUMN IF EXISTS institution;
                ALTER TABLE courses DROP COLUMN IF EXISTS level;
                "#,
            )
            .await?;
        Ok(())
    }
}
