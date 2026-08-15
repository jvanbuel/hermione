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
                -- A course may be linked to a git repository (the assignment /
                -- template repo it was created from). Optional: absence just means
                -- the course isn't backed by a repo.
                ALTER TABLE courses ADD COLUMN IF NOT EXISTS repo_url TEXT;
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
                ALTER TABLE courses DROP COLUMN IF EXISTS repo_url;
                "#,
            )
            .await?;
        Ok(())
    }
}
