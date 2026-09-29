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

impl fmt::Display for Student {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
