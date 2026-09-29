//! Which words changed inside a line that was edited.
//!
//! A row that was removed and the row that replaced it usually differ in a few
//! words — `x = foo(a, b)` became `x = foo(a, c)` — and a teacher scanning a
//! student's diff is really asking "what did they change?". Washing the whole
//! line answers "these lines", so the words that differ are marked too, as
//! GitHub's diff does.
//!
//! The answer is byte ranges into each line, at word granularity: a run of
//! letters, digits and underscores is one token, a run of whitespace is one, and
//! every other character is its own. That is what makes `b)` → `c)` mark `b`
//! and `c` rather than the parenthesis with them.

use std::ops::Range;

use similar::{capture_diff_slices, Algorithm, DiffTag};

/// Lines longer than this aren't compared. Comparing is cheap, but a marked
/// span in a very long line is unreadable and the line is usually generated.
const MAX_LINE: usize = 400;

/// What changed between an old line and the line that replaced it.
#[derive(Debug, PartialEq, Eq)]
pub struct Changes {
    /// Byte ranges of the old line that are gone.
    pub old: Vec<Range<usize>>,
    /// Byte ranges of the new line that are new.
    pub new: Vec<Range<usize>>,
}

/// The words that differ between two versions of a line, or `None` when
/// marking them wouldn't say anything: the lines are the same, they share too
/// little for "the words that changed" to mean something (a rewritten line is
/// better left to the row's own wash), or marking would cover a whole line.
pub fn changes(old: &str, new: &str) -> Option<Changes> {
    if old == new || old.len() > MAX_LINE || new.len() > MAX_LINE {
        return None;
    }
    let (old_tokens, new_tokens) = (tokens(old), tokens(new));
    let old_words: Vec<&str> = old_tokens.iter().map(|r| &old[r.clone()]).collect();
    let new_words: Vec<&str> = new_tokens.iter().map(|r| &new[r.clone()]).collect();

    let (mut gone, mut added) = (Vec::new(), Vec::new());
    let mut shared = 0;
    for op in capture_diff_slices(Algorithm::Myers, &old_words, &new_words) {
        let (tag, old_at, new_at) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {
                shared += span(&old_tokens, old_at).map_or(0, |r| weight(&old[r]));
            }
            DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                gone.extend(span(&old_tokens, old_at));
                added.extend(span(&new_tokens, new_at));
            }
        }
    }

    // Most of the shorter line has to survive for the difference to be a
    // difference *within* it. Whitespace isn't counted on either side: two
    // unrelated lines laid out alike would otherwise look like edits.
    if shared * 2 < weight(old).min(weight(new)) {
        return None;
    }
    let (old_marks, new_marks) = (join(gone, old), join(added, new));
    if covers(&old_marks, old) || covers(&new_marks, new) {
        return None;
    }
    Some(Changes {
        old: old_marks,
        new: new_marks,
    })
}

/// How much of a line is text rather than spacing, in bytes.
fn weight(text: &str) -> usize {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(char::len_utf8)
        .sum()
}

/// The byte range a run of tokens occupies, or `None` for an empty run.
fn span(tokens: &[Range<usize>], run: Range<usize>) -> Option<Range<usize>> {
    Some(tokens.get(run.start)?.start..tokens.get(run.end.checked_sub(1)?)?.end)
}

/// Splits a line into words, whitespace runs and single symbols, as byte ranges.
fn tokens(line: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Kind {
        Word,
        Space,
        Symbol,
    }
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Kind::Word
        } else if c.is_whitespace() {
            Kind::Space
        } else {
            Kind::Symbol
        }
    };

    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<Kind> = None;
    for (at, c) in line.char_indices() {
        let k = kind(c);
        match out.last_mut() {
            // Symbols never merge: `)` and `,` are separate things to change.
            Some(run) if last.as_ref() == Some(&k) && k != Kind::Symbol => {
                run.end = at + c.len_utf8();
            }
            _ => out.push(at..at + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

/// Joins ranges that have nothing but whitespace between them: `foo bar`
/// changed to `baz qux` is one changed phrase, not two words and a gap.
fn join(ranges: Vec<Range<usize>>, line: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(prev) if line[prev.end..r.start].chars().all(char::is_whitespace) => {
                prev.end = r.end;
            }
            _ => out.push(r),
        }
    }
    out
}

/// Whether the ranges leave nothing of the line unmarked (ignoring the
/// whitespace around it).
fn covers(ranges: &[Range<usize>], line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let start = line.len() - line.trim_start().len();
    let end = start + trimmed.len();
    ranges.iter().any(|r| r.start <= start && r.end >= end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marked parts of each line, as text.
    fn marked(old: &str, new: &str) -> Option<(Vec<String>, Vec<String>)> {
        let c = changes(old, new)?;
        let text = |line: &str, rs: &[Range<usize>]| -> Vec<String> {
            rs.iter().map(|r| line[r.clone()].to_string()).collect()
        };
        Some((text(old, &c.old), text(new, &c.new)))
    }

    #[test]
    fn marks_the_word_that_changed_and_nothing_around_it() {
        let (old, new) = marked("x = foo(a, b)", "x = foo(a, c)").unwrap();
        assert_eq!((old, new), (vec!["b".into()], vec!["c".into()]));
    }

    #[test]
    fn a_word_added_is_marked_only_on_the_new_side() {
        let (old, new) = marked("return total", "return total + tax").unwrap();
        assert!(old.is_empty(), "{old:?}");
        assert_eq!(new.concat().trim(), "+ tax");
    }

    #[test]
    fn a_changed_phrase_is_one_mark_not_a_word_and_a_gap() {
        let (old, new) = marked("let a = one two;", "let a = red blue;").unwrap();
        assert_eq!(
            (old, new),
            (vec!["one two".into()], vec!["red blue".into()])
        );
    }

    #[test]
    fn symbols_are_their_own_tokens() {
        // `b)` -> `c)` must not drag the parenthesis into the mark.
        let (old, _) = marked("f(a, b)", "f(a, c)").unwrap();
        assert_eq!(old, vec!["b".to_string()]);
    }

    #[test]
    fn a_rewritten_line_is_left_to_the_row_wash() {
        assert_eq!(marked("total = price * count", "print('done')"), None);
    }

    #[test]
    fn a_pair_sharing_less_than_half_is_not_an_edit_of_one_line() {
        // They share `a = ` and little else, and the differing part doesn't
        // reach the ends, so only the similarity rule can refuse this pair.
        assert_eq!(marked("a = b + c + d + e", "a = x * y * z - w"), None);
    }

    #[test]
    fn a_pair_sharing_most_of_its_text_is_marked() {
        assert!(marked("a = b + c + d + e", "a = b + c + d + f").is_some());
    }

    #[test]
    fn identical_lines_have_nothing_to_mark() {
        assert_eq!(changes("same", "same"), None);
    }

    #[test]
    fn a_change_that_covers_the_whole_line_marks_nothing() {
        // Marking every word says no more than the wash already does.
        assert_eq!(marked("    foo", "    bar"), None);
    }

    #[test]
    fn ranges_land_on_character_boundaries() {
        // Multi-byte text must not be cut mid-character.
        let (old, new) = marked("s = \"héllo wörld\" + 1", "s = \"héllo wörld\" + 2").unwrap();
        assert_eq!((old, new), (vec!["1".into()], vec!["2".into()]));
    }

    #[test]
    fn very_long_lines_are_not_compared() {
        let long = "x".repeat(MAX_LINE + 1);
        assert_eq!(changes(&long, &format!("{long}y")), None);
    }
}
