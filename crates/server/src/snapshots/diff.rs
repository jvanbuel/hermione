//! A student's working changes, ready to draw.
//!
//! The dashboard is sent [`Row`]s, not raw diff lines: each one already knows
//! its sign, its line numbers and its own spans, so the page has nothing to
//! look up or count. A row's shape follows from its sign — an added line has no
//! old line number, a removed line has no new one — so the sign and the numbers
//! can't contradict each other, which they could if each were its own field.

use serde::Serialize;

use super::model::is_false;
use super::report::{self, DiffLine};
use crate::highlight::{Grammar, Highlighted, Line};

/// Most rows one snapshot carries. A wholesale rewrite of a large file would
/// otherwise put the whole file in every poll's response.
const MAX_ROWS: usize = 2000;

/// How the buffer was coloured, when it could be: its spans, and the grammar
/// they came from. Removed lines aren't in the buffer, so they are coloured
/// with the same grammar. Bundled so "the buffer was coloured but with what?"
/// is not a state a caller can be in.
#[derive(Clone, Copy)]
pub struct Colour<'a> {
    pub buffer: &'a Highlighted,
    pub grammar: Grammar,
}

/// The buffer's differences from the last commit.
///
/// The counts are worked out here from the lines, never taken from the editor,
/// so they can't disagree with them. They cover the whole change even when
/// `truncated` says only the start of it is sent.
#[derive(Debug, Serialize)]
pub struct Diff {
    added: u32,
    removed: u32,
    hunks: Vec<Hunk>,
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
}

impl Diff {
    pub fn new(hunks: Vec<report::Hunk>, colour: Option<Colour<'_>>) -> Self {
        let (mut added, mut removed) = (0, 0);
        for line in hunks.iter().flat_map(|h| &h.lines) {
            match line {
                DiffLine::Added(_) => added += 1,
                DiffLine::Removed(_) => removed += 1,
                DiffLine::Context(_) => {}
            }
        }

        // Once the budget runs out, everything after it goes too: a diff with a
        // hole in the middle reads as if it were complete.
        let mut budget = MAX_ROWS;
        let mut truncated = false;
        let mut kept = Vec::new();
        for hunk in hunks {
            if truncated || hunk.lines.len() > budget {
                truncated = true;
                continue;
            }
            budget -= hunk.lines.len();
            kept.push(Hunk::new(hunk, colour));
        }
        Self {
            added,
            removed,
            hunks: kept,
            truncated,
        }
    }
}

/// A run of changes with the context around it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    rows: Vec<Row>,
}

/// One line of a hunk.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "sign")]
pub enum Row {
    #[serde(rename = " ")]
    Context { old: u32, new: u32, spans: Line },
    #[serde(rename = "+")]
    Added { new: u32, spans: Line },
    #[serde(rename = "-")]
    Removed { old: u32, spans: Line },
}

impl Hunk {
    /// Turns a hunk's lines into rows.
    ///
    /// Context and added lines are lines of the buffer, so their spans are read
    /// out of it by line number and get the whole file's parse context for
    /// free. Removed lines exist only in the last commit, which we never see, so
    /// they are the one side coloured separately: the hunk's old side —
    /// context and removed lines, in order — is parsed as one fragment, so the
    /// parser sees whatever surroundings the hunk carries.
    ///
    /// A row uses spans only if they rebuild its text exactly, otherwise it is
    /// plain. A buffer clamped at the size cap has fewer lines than its diff
    /// refers to, and drawing text a student never typed is the one failure
    /// worth guarding against. Doing the check here means it happens once per
    /// snapshot, not once per row per poll in the browser.
    fn new(hunk: report::Hunk, colour: Option<Colour<'_>>) -> Self {
        let has_removed = hunk.lines.iter().any(|l| matches!(l, DiffLine::Removed(_)));
        let mut old_side = colour
            .filter(|_| has_removed)
            .and_then(|c| {
                c.grammar
                    .highlight(hunk.lines.iter().filter_map(DiffLine::old_text))
                    .ok()
            })
            .map(IntoIterator::into_iter);

        let buffer_line = |number: u32, text: &str| {
            let candidate = colour.and_then(|c| c.buffer.line(number)).cloned();
            spans(candidate, text)
        };

        let (mut old, mut new) = (hunk.old_start, hunk.new_start);
        let mut rows = Vec::with_capacity(hunk.lines.len());
        for line in &hunk.lines {
            rows.push(match line {
                DiffLine::Context(text) => {
                    // Keep the old-side cursor level with the buffer's.
                    old_side.as_mut().and_then(Iterator::next);
                    let row = Row::Context {
                        old,
                        new,
                        spans: buffer_line(new, text),
                    };
                    (old, new) = (old + 1, new + 1);
                    row
                }
                DiffLine::Added(text) => {
                    let row = Row::Added {
                        new,
                        spans: buffer_line(new, text),
                    };
                    new += 1;
                    row
                }
                DiffLine::Removed(text) => {
                    let candidate = old_side.as_mut().and_then(Iterator::next);
                    let row = Row::Removed {
                        old,
                        spans: spans(candidate, text),
                    };
                    old += 1;
                    row
                }
            });
        }

        let count = |keep: fn(&Row) -> bool| rows.iter().filter(|r| keep(r)).count() as u32;
        Self {
            old_start: hunk.old_start,
            old_lines: count(|r| !matches!(r, Row::Added { .. })),
            new_start: hunk.new_start,
            new_lines: count(|r| !matches!(r, Row::Removed { .. })),
            rows,
        }
    }
}

/// `candidate` if it really is `text`'s spans, otherwise `text` plain.
fn spans(candidate: Option<Line>, text: &str) -> Line {
    candidate
        .filter(|l| l.rebuilds(text))
        .unwrap_or_else(|| Line::plain(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Class;

    fn python() -> Grammar {
        Grammar::detect("python", "x.py").unwrap()
    }

    fn hunk(old_start: u32, new_start: u32, lines: &[&str]) -> report::Hunk {
        report::Hunk {
            old_start,
            new_start,
            lines: lines
                .iter()
                .map(|l| report::DiffLine::try_from(l.to_string()).unwrap())
                .collect(),
        }
    }

    fn text(spans: &Line) -> String {
        spans.spans().iter().map(|s| s.text()).collect()
    }

    fn rows(diff: &Diff) -> &[Row] {
        &diff.hunks[0].rows
    }

    #[test]
    fn rows_carry_their_numbers_and_their_own_spans() {
        // The buffer is what the student sees now: the removed lines are not in it.
        let grammar = python();
        let buffer = grammar
            .highlight("def f():\n    return 2\n".split('\n'))
            .unwrap();
        let colour = Colour {
            buffer: &buffer,
            grammar,
        };
        let diff = Diff::new(
            vec![hunk(
                1,
                1,
                &[
                    " def f():",
                    "-    return 1",
                    "-    # gone",
                    "+    return 2",
                    " ",
                ],
            )],
            Some(colour),
        );

        let [context, gone_a, gone_b, added, blank] = rows(&diff) else {
            panic!("five rows");
        };
        assert!(matches!(context, Row::Context { old: 1, new: 1, .. }));
        assert!(matches!(gone_a, Row::Removed { old: 2, .. }));
        assert!(matches!(gone_b, Row::Removed { old: 3, .. }));
        assert!(matches!(added, Row::Added { new: 2, .. }));
        assert!(matches!(blank, Row::Context { old: 4, new: 3, .. }));

        let Row::Removed { spans: comment, .. } = gone_b else {
            unreachable!()
        };
        assert_eq!(text(comment), "    # gone");
        // A removed comment — which exists in no buffer — is still a comment.
        assert!(comment.spans().iter().any(|s| s.class() == Class::Comment));
        // An added line takes the buffer's spans, keyword and all.
        let Row::Added { spans: ret, .. } = added else {
            unreachable!()
        };
        assert!(ret.spans().iter().any(|s| s.class() == Class::Keyword));

        // Everything a unified-diff header would say is derived from the rows.
        let h = &diff.hunks[0];
        assert_eq!((h.old_lines, h.new_lines), (4, 3));
        assert_eq!((diff.added, diff.removed), (1, 2));
    }

    #[test]
    fn rows_are_plain_when_there_is_nothing_to_colour() {
        let diff = Diff::new(vec![hunk(1, 1, &[" x = 1", "-y = 2", "+y = 3"])], None);
        for row in rows(&diff) {
            let (Row::Context { spans, .. }
            | Row::Added { spans, .. }
            | Row::Removed { spans, .. }) = row;
            assert_eq!(spans.spans().len(), 1, "{row:?}");
            assert_eq!(spans.spans()[0].class(), Class::Plain);
        }
    }

    #[test]
    fn spans_that_do_not_belong_to_a_line_are_not_used() {
        // A buffer clamped short: the diff's second line points past the end,
        // and its first line's spans are for different text entirely.
        let grammar = python();
        let buffer = grammar.highlight(["z = 9"]).unwrap();
        let diff = Diff::new(
            vec![hunk(1, 1, &[" a = 1", "+b = 2"])],
            Some(Colour {
                buffer: &buffer,
                grammar,
            }),
        );
        for row in rows(&diff) {
            let (Row::Context { spans, .. }
            | Row::Added { spans, .. }
            | Row::Removed { spans, .. }) = row;
            assert_eq!(spans.spans().len(), 1, "fell back to plain: {row:?}");
        }
    }

    #[test]
    fn rows_serialize_with_the_sign_as_the_tag() {
        let diff = Diff::new(vec![hunk(3, 3, &[" a", "+b", "-c"])], None);
        let json = serde_json::to_value(&diff).unwrap();
        let rows = &json["hunks"][0]["rows"];
        assert_eq!(rows[0]["sign"], " ");
        assert_eq!(
            (rows[1]["sign"].clone(), rows[1]["new"].clone()),
            ("+".into(), 4.into())
        );
        assert!(
            rows[1].get("old").is_none(),
            "an added line has no old number"
        );
        assert!(
            rows[2].get("new").is_none(),
            "a removed line has no new number"
        );
        assert!(json["hunks"][0].get("lines").is_none());
        assert!(json.get("truncated").is_none());
    }

    #[test]
    fn a_big_diff_keeps_its_start_and_says_so() {
        let big = |n: usize| hunk(1, 1, &vec!["+x"; n]);
        let diff = Diff::new(vec![big(MAX_ROWS), big(1), big(1)], None);
        assert_eq!(
            diff.hunks.len(),
            1,
            "everything after the overflow goes too"
        );
        assert!(diff.truncated);
        assert_eq!(
            diff.added as usize,
            MAX_ROWS + 2,
            "counts cover the whole change"
        );
    }

    #[test]
    fn a_diff_that_fits_is_not_marked_truncated() {
        let diff = Diff::new(vec![hunk(1, 1, &["+x"])], None);
        assert!(!diff.truncated);
    }
}
