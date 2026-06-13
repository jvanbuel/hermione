//! Database migrations for Hermione.

pub use sea_orm_migration::prelude::*;

mod m20240101_000001_create_tables;
mod m20240101_000002_add_event_text;
mod m20240101_000003_create_file_events;
mod m20240101_000004_index_file_events_at;
mod m20240101_000005_multi_tenancy;
mod m20240101_000006_session_error_count;
mod m20240101_000007_create_messages;
mod m20240101_000008_create_exercises;
mod m20240101_000009_file_event_provenance;
mod m20240101_000010_index_sessions_course_started;
mod m20240101_000011_assistant;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240101_000001_create_tables::Migration),
            Box::new(m20240101_000002_add_event_text::Migration),
            Box::new(m20240101_000003_create_file_events::Migration),
            Box::new(m20240101_000004_index_file_events_at::Migration),
            Box::new(m20240101_000005_multi_tenancy::Migration),
            Box::new(m20240101_000006_session_error_count::Migration),
            Box::new(m20240101_000007_create_messages::Migration),
            Box::new(m20240101_000008_create_exercises::Migration),
            Box::new(m20240101_000009_file_event_provenance::Migration),
            Box::new(m20240101_000010_index_sessions_course_started::Migration),
            Box::new(m20240101_000011_assistant::Migration),
        ]
    }
}
