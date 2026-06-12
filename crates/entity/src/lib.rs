//! SeaORM entities for Hermione's Postgres schema.

pub mod admins;
pub mod course_admins;
pub mod courses;
pub mod file_events;
pub mod messages;
pub mod sessions;
pub mod terminal_events;

pub use admins::Entity as Admins;
pub use course_admins::Entity as CourseAdmins;
pub use courses::Entity as Courses;
pub use file_events::Entity as FileEvents;
pub use messages::Entity as Messages;
pub use sessions::Entity as Sessions;
pub use terminal_events::Entity as TerminalEvents;
