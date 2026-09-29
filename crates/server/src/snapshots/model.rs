//! One student's editor at one moment, as the server keeps and shows it.
//!
//! This is the *outbound* model. What an editor sends is [`super::report`], and
//! the two are separate types on purpose: a report is a claim, this is what we
//! made of it — clamped to size, highlighted, its diff turned into rows. There
//! is no half-processed state to hold, and a client can't send us something
//! that looks already-processed.
//!
//! The states are sum types rather than a struct of optionals, so the
//! combinations that make no sense — `declined` alongside `content`, a diff
//! against an untracked file, a highlight with no text to have come from — have
//! no way to be written down.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::diff::{Colour, Diff};
use super::report;
use crate::highlight::{Grammar, Highlighted};

/// Most buffer text the server will hold, whatever the editor sent.
const MAX_CONTENT_BYTES: usize = 256 * 1024;

pub(super) fn is_false(b: &bool) -> bool {
    !*b
}

/// What a student's editor answered with.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Snapshot {
    /// Their configuration forbids sharing file contents.
    Declined,
    /// Sharing is on, but no file is open.
    Empty,
    /// Boxed: the other states carry nothing, and a file is a couple of hundred bytes.
    File(Box<File>),
}

/// Where the caret is. Both are 1-based, so neither can be zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub line: NonZeroU32,
    pub column: NonZeroU32,
}

/// The file a student has open.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    path: String,
    relative_path: String,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exercise: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<Cursor>,
    /// The buffer has unsaved changes, so it differs from the file on disk.
    dirty: bool,
    #[serde(flatten)]
    text: Text,
    /// `text` as one list of classed spans per line. Absent when the language
    /// is unknown or the file too long; the page then draws it plain.
    #[serde(skip_serializing_if = "Option::is_none")]
    highlight: Option<Highlighted>,
    baseline: Baseline,
}

/// What the buffer is compared against.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Baseline {
    Head(Diff),
    Untracked,
    #[serde(rename = "none")]
    Unavailable,
}

/// The buffer's text, never over the size cap.
#[derive(Debug, Serialize)]
struct Text {
    content: String,
    /// The text was cut short, here or by the editor.
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
}

impl Text {
    fn new(mut content: String, cut_by_editor: bool) -> Self {
        let too_long = content.len() > MAX_CONTENT_BYTES;
        if too_long {
            // Cut on a char boundary so the result stays valid UTF-8. Byte 0
            // always is one, so this terminates.
            let mut end = MAX_CONTENT_BYTES;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
        }
        Self {
            content,
            truncated: cut_by_editor || too_long,
        }
    }

    /// Split exactly as the browser splits it, so line *n* here is line *n* there.
    fn lines(&self) -> impl Iterator<Item = &str> {
        self.content.split('\n')
    }
}

impl From<report::State> for Snapshot {
    /// Does the work of turning a report into something to show. It parses the
    /// whole buffer, which is tens to hundreds of milliseconds of solid CPU:
    /// call it off the async runtime's worker threads.
    fn from(state: report::State) -> Self {
        match state {
            report::State::Declined => Self::Declined,
            report::State::Empty => Self::Empty,
            report::State::File(file) => Self::File(Box::new(File::from(file))),
        }
    }
}

impl From<report::File> for File {
    fn from(f: report::File) -> Self {
        let text = Text::new(f.content, f.truncated);
        let grammar = Grammar::detect(&f.language, &f.relative_path);
        let highlight = grammar.and_then(|g| g.highlight(text.lines()).ok());
        let colour = grammar
            .zip(highlight.as_ref())
            .map(|(grammar, buffer)| Colour { buffer, grammar });

        let baseline = match f.baseline {
            report::Baseline::Head { hunks } => Baseline::Head(Diff::new(hunks, colour)),
            report::Baseline::Untracked => Baseline::Untracked,
            report::Baseline::Unavailable => Baseline::Unavailable,
        };
        Self {
            path: f.path,
            relative_path: f.relative_path,
            language: f.language,
            exercise: f.exercise,
            cursor: f.cursor,
            dirty: f.dirty,
            text,
            highlight,
            baseline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn snapshot(v: Value) -> Value {
        let report: report::Report = serde_json::from_value(v).unwrap();
        serde_json::to_value(Snapshot::from(report.state)).unwrap()
    }

    fn file(content: &str, baseline: Value) -> Value {
        json!({
            "student": "alice", "state": "file",
            "path": "/w/ex1/main.py", "relativePath": "ex1/main.py", "language": "python",
            "cursor": {"line": 1, "column": 3}, "dirty": true,
            "content": content, "baseline": baseline,
        })
    }

    #[test]
    fn a_file_is_shown_with_its_text_cursor_and_spans() {
        let s = snapshot(file("x = 1\n", json!({"kind": "untracked"})));
        assert_eq!(s["state"], "file");
        assert_eq!(s["content"], "x = 1\n");
        assert_eq!(s["cursor"], json!({"line": 1, "column": 3}));
        assert_eq!(s["dirty"], true);
        assert_eq!(s["baseline"], json!({"kind": "untracked"}));
        assert!(s.get("truncated").is_none());
        // One span list per screen line, the trailing newline's empty line included.
        assert_eq!(s["highlight"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn declined_and_empty_have_nothing_to_show() {
        for state in ["declined", "empty"] {
            let s = snapshot(json!({"student": "a", "state": state}));
            assert_eq!(s, json!({ "state": state }));
        }
    }

    #[test]
    fn a_head_baseline_is_a_diff_with_derived_counts() {
        let s = snapshot(file(
            "a\nc\n",
            json!({"kind": "head", "hunks": [
                {"oldStart": 1, "newStart": 1, "lines": [" a", "-b", "+c"]}
            ]}),
        ));
        let b = &s["baseline"];
        assert_eq!(b["kind"], "head");
        assert_eq!(
            (b["added"].clone(), b["removed"].clone()),
            (1.into(), 1.into())
        );
        assert_eq!(b["hunks"][0]["rows"][1]["sign"], "-");
        assert!(
            b["hunks"][0].get("lines").is_none(),
            "raw lines are not sent on"
        );
    }

    #[test]
    fn oversized_text_is_cut_on_a_char_boundary_and_says_so() {
        // A multi-byte char straddling the cap must not be cut in half.
        let big = "é".repeat(MAX_CONTENT_BYTES);
        let text = Text::new(big, false);
        assert!(text.content.len() <= MAX_CONTENT_BYTES);
        assert!(text.truncated);
        assert!(text.content.chars().all(|c| c == 'é'));
    }

    #[test]
    fn text_that_fits_is_left_alone() {
        let text = Text::new("print('hi')".into(), false);
        assert_eq!(text.content, "print('hi')");
        assert!(!text.truncated);
    }

    #[test]
    fn a_cut_by_the_editor_is_remembered() {
        assert!(Text::new("short".into(), true).truncated);
    }

    #[test]
    fn a_language_we_cannot_colour_is_still_shown() {
        let mut v = file("hello\n", json!({"kind": "none"}));
        v["language"] = json!("nonesuch");
        v["relativePath"] = json!("README.nonesuch");
        let s = snapshot(v);
        assert_eq!(s["content"], "hello\n");
        assert!(s.get("highlight").is_none());
        assert_eq!(s["baseline"], json!({"kind": "none"}));
    }
}
