//! What a student's editor posts to `/api/file-snapshots`.
//!
//! These types only ever come out of a `Deserialize`, and only carry what an
//! editor can truthfully know. Anything derivable — a diff's added and removed
//! counts, a hunk's line counts, whether the text was cut short — is the
//! server's to work out (see [`super::model`]), so a client can't send counts
//! that disagree with the lines beside them.
//!
//! Each shape is a sum type, not a struct of optionals: an editor that has
//! declined to share has no `content` field to send, so there is nothing for a
//! buggy one to send alongside `declined`.

use serde::Deserialize;

use super::model::Cursor;
use crate::student::Student;

/// One editor's answer to a snapshot request.
#[derive(Debug, Deserialize)]
pub struct Report {
    pub student: Student,
    #[serde(flatten)]
    pub state: State,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    /// The student's configuration forbids sharing file contents.
    Declined,
    /// Sharing is on, but no file is open.
    Empty,
    /// The buffer is exactly as it was in the snapshot the server numbered
    /// `basis`, and only the caret has moved. A cheap thing to send while a
    /// teacher watches someone think: the server keeps everything else, and
    /// refuses (409) if `basis` is no longer its latest, so the editor knows to
    /// send the whole file instead.
    Cursor {
        basis: u64,
        cursor: Cursor,
    },
    File(File),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub path: String,
    pub relative_path: String,
    /// VSCode's language id for the document.
    pub language: String,
    pub exercise: Option<String>,
    pub cursor: Option<Cursor>,
    /// The buffer has unsaved changes.
    #[serde(default)]
    pub dirty: bool,
    /// The buffer as the student sees it this instant.
    pub content: String,
    /// The editor already cut `content` short before sending it.
    #[serde(default)]
    pub truncated: bool,
    pub baseline: Baseline,
}

/// What the buffer is compared against.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Baseline {
    /// The last commit, with the buffer's differences from it.
    Head { hunks: Vec<Hunk> },
    /// The file has never been committed.
    Untracked,
    /// There is nothing to compare with: a notebook cell, or no git.
    #[serde(rename = "none")]
    Unavailable,
}

/// One run of changes. The counts a unified diff also carries are left out on
/// purpose: they follow from `lines`, and are worked out from them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// 1-based line in the committed file this hunk begins at.
    pub old_start: u32,
    /// 1-based line in the buffer this hunk begins at.
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

/// One line of a hunk, with its sign taken off.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// A diff line whose first character wasn't `' '`, `'+'` or `'-'`.
#[derive(Debug, PartialEq, Eq)]
pub struct BadSign(Option<char>);

impl std::fmt::Display for BadSign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(c) => write!(f, "diff line starts with {c:?}, not ' ', '+' or '-'"),
            None => f.write_str("diff line is empty"),
        }
    }
}

impl TryFrom<String> for DiffLine {
    type Error = BadSign;

    fn try_from(line: String) -> Result<Self, BadSign> {
        let mut chars = line.chars();
        let sign = chars.next();
        let text = chars.as_str().to_string();
        match sign {
            Some(' ') => Ok(Self::Context(text)),
            Some('+') => Ok(Self::Added(text)),
            Some('-') => Ok(Self::Removed(text)),
            other => Err(BadSign(other)),
        }
    }
}

impl DiffLine {
    /// The line's text in the committed file, or `None` if it isn't there.
    pub fn old_text(&self) -> Option<&str> {
        match self {
            Self::Context(t) | Self::Removed(t) => Some(t),
            Self::Added(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Result<Report, serde_json::Error> {
        serde_json::from_value(v)
    }

    fn file() -> serde_json::Value {
        json!({
            "student": "alice", "state": "file",
            "path": "/w/ex1/main.py", "relativePath": "ex1/main.py", "language": "python",
            "cursor": {"line": 3, "column": 9},
            "content": "x = 1\n",
            "baseline": {"kind": "head", "hunks": [
                {"oldStart": 1, "newStart": 1, "lines": [" a", "-b", "+c"]}
            ]}
        })
    }

    #[test]
    fn a_whole_report_parses() {
        let Report {
            student,
            state: State::File(f),
        } = parse(file()).unwrap()
        else {
            panic!("expected a file report");
        };
        assert_eq!(student.to_string(), "alice");
        assert_eq!(f.cursor.unwrap().line.get(), 3);
        assert!(!f.dirty && !f.truncated, "flags default to false");
        let Baseline::Head { hunks } = f.baseline else {
            panic!("head")
        };
        assert_eq!(
            hunks[0].lines,
            [
                DiffLine::Context("a".into()),
                DiffLine::Removed("b".into()),
                DiffLine::Added("c".into())
            ]
        );
    }

    #[test]
    fn declined_and_empty_carry_nothing_else() {
        for state in ["declined", "empty"] {
            // Stray fields a buggy editor might send are simply not read.
            let r = parse(json!({"student": "a", "state": state, "content": "leak"})).unwrap();
            assert!(!matches!(r.state, State::File(_)), "{state}");
        }
    }

    #[test]
    fn things_that_cannot_be_true_are_refused_at_the_door() {
        let mut blank = file();
        blank["student"] = json!("  ");
        assert!(parse(blank).is_err(), "blank student");

        let mut zero = file();
        zero["cursor"] = json!({"line": 0, "column": 1});
        assert!(parse(zero).is_err(), "line 0");

        let mut sign = file();
        sign["baseline"]["hunks"][0]["lines"] = json!(["?oops"]);
        assert!(parse(sign).is_err(), "bad sign");

        let mut empty_line = file();
        empty_line["baseline"]["hunks"][0]["lines"] = json!([""]);
        assert!(parse(empty_line).is_err(), "empty diff line");

        let mut state = file();
        state["state"] = json!("maybe");
        assert!(parse(state).is_err(), "unknown state");

        let mut base = file();
        base["baseline"] = json!({"kind": "sideways"});
        assert!(parse(base).is_err(), "unknown baseline");
    }

    #[test]
    fn a_file_needs_a_baseline_and_content() {
        for missing in ["baseline", "content", "relativePath"] {
            let mut v = file();
            v.as_object_mut().unwrap().remove(missing);
            assert!(parse(v).is_err(), "{missing}");
        }
    }

    #[test]
    fn only_context_and_removed_lines_exist_in_the_old_file() {
        assert_eq!(DiffLine::Context("x".into()).old_text(), Some("x"));
        assert_eq!(DiffLine::Removed("y".into()).old_text(), Some("y"));
        assert_eq!(DiffLine::Added("z".into()).old_text(), None);
    }
}
