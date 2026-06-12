//! Database migrations for Hermione.

pub use sea_orm_migration::prelude::*;

mod m20240101_000001_create_tables;
mod m20240101_000002_add_event_text;
mod m20240101_000003_create_file_events;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240101_000001_create_tables::Migration),
            Box::new(m20240101_000002_add_event_text::Migration),
            Box::new(m20240101_000003_create_file_events::Migration),
        ]
    }
}
