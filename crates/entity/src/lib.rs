//! SeaORM entities for Hermione's Postgres schema.

pub mod admins;
pub mod assistant_conversations;
pub mod assistant_messages;
pub mod course_admins;
pub mod course_assistants;
pub mod courses;
pub mod exercises;
pub mod file_events;
pub mod messages;
pub mod sessions;
pub mod terminal_events;

pub use admins::Entity as Admins;
pub use assistant_conversations::Entity as AssistantConversations;
pub use assistant_messages::Entity as AssistantMessages;
pub use course_admins::Entity as CourseAdmins;
pub use course_assistants::Entity as CourseAssistants;
pub use courses::Entity as Courses;
pub use exercises::Entity as Exercises;
pub use file_events::Entity as FileEvents;
pub use messages::Entity as Messages;
pub use sessions::Entity as Sessions;
pub use terminal_events::Entity as TerminalEvents;
