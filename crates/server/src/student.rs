//! Who a piece of live, per-student state belongs to.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A student's name. Never blank.
///
/// State kept per student is keyed by name, and a blank one would file every
/// unidentified editor under the same slot. The type refuses to hold one, so
/// the handlers that take a name no longer each have to remember to check —
/// and can't forget to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Student(String);

/// A student name that was empty or only whitespace.
#[derive(Debug, PartialEq, Eq)]
pub struct BlankName;

impl fmt::Display for BlankName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("student name is blank")
    }
}

impl std::error::Error for BlankName {}

impl TryFrom<String> for Student {
    type Error = BlankName;

    fn try_from(name: String) -> Result<Self, BlankName> {
        if name.trim().is_empty() {
            Err(BlankName)
        } else {
            Ok(Self(name))
        }
    }
}

impl Student {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Student {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who a message is for.
///
/// A sum type rather than `Option<Student>`: "no student" reads as "nobody",
/// and the difference between nobody and everybody is exactly the kind of thing
/// that is got backwards once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audience {
    /// Every student in the course, and the teachers' dashboard.
    Everyone,
    /// One student's editor, and nobody else's — not even a teacher's.
    Student(Student),
}

impl Audience {
    /// Whether a socket belonging to `listener` should be sent the message.
    /// `None` is a listener with no student: the dashboard.
    pub fn reaches(&self, listener: Option<&Student>) -> bool {
        match self {
            Self::Everyone => true,
            Self::Student(only) => listener == Some(only),
        }
    }
}

/// One student in one course: the unit that live state — their editor's control
/// channel, their latest snapshot — is kept per. A student named `alice` in one
/// course is a different slot from `alice` in another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Slot {
    pub course: Uuid,
    pub student: Student,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn student(name: &str) -> Student {
        Student::try_from(name.to_string()).unwrap()
    }

    #[test]
    fn everyone_reaches_everyone_and_a_student_only_that_student() {
        let ada = student("ada");
        assert!(Audience::Everyone.reaches(None));
        assert!(Audience::Everyone.reaches(Some(&ada)));
        let only_ada = Audience::Student(ada.clone());
        assert!(only_ada.reaches(Some(&ada)));
        assert!(!only_ada.reaches(Some(&student("bo"))));
        // Not the dashboard either: a private word to one student stays private.
        assert!(!only_ada.reaches(None));
    }

    #[test]
    fn blank_names_are_refused() {
        assert_eq!(Student::try_from(String::new()), Err(BlankName));
        assert_eq!(Student::try_from("  \t".to_string()), Err(BlankName));
        assert_eq!(
            Student::try_from("alice".to_string()).unwrap().to_string(),
            "alice"
        );
    }

    #[test]
    fn a_blank_name_cannot_be_deserialized_either() {
        assert!(serde_json::from_str::<Student>("\"\"").is_err());
        assert_eq!(
            serde_json::from_str::<Student>("\"bob\"")
                .unwrap()
                .to_string(),
            "bob"
        );
    }

    #[test]
    fn serializes_as_the_bare_name() {
        let s = Student::try_from("alice".to_string()).unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"alice\"");
    }

    #[test]
    fn the_same_name_in_two_courses_is_two_slots() {
        let alice = || Student::try_from("alice".to_string()).unwrap();
        let a = Slot {
            course: Uuid::new_v4(),
            student: alice(),
        };
        let b = Slot {
            course: Uuid::new_v4(),
            student: alice(),
        };
        assert_ne!(a, b);
    }
}
