//! A student's buffer against a teacher's reference solution.
//!
//! The comparison is made here, on the server, and only ever answered to a
//! teacher: the whole point of a reference solution is that a student's editor
//! never receives it. That is also why the diff is computed from the two texts
//! rather than reported by the editor, as the diff against the last commit is —
//! the editor doesn't have the solution to diff against.
//!
//! The result reuses [`Diff`], the same rows the commit diff is drawn from, with
//! the solution as the "old" side: a `-` row is a line the solution has that the
//! student's file lacks, a `+` row a line the student has that the solution
//! doesn't. So colouring, word marks and the row cap all work unchanged.

use serde::Serialize;
use similar::{Algorithm, ChangeTag, TextDiff};

use super::diff::Diff;
use super::report::{DiffLine, Hunk};
use crate::solutions::{FetchError, GitRef, RepoPath};

/// Lines of unchanged context kept around each difference, as the commit diff does.
const CONTEXT: usize = 3;

/// Which file in the repo was compared with, so a teacher can see it.
#[derive(Debug, Serialize)]
pub struct Located {
    path: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    git_ref: Option<String>,
}

impl Located {
    pub fn new(path: &RepoPath, git_ref: Option<&GitRef>) -> Self {
        Self {
            path: path.to_string(),
            git_ref: git_ref.map(ToString::to_string),
        }
    }
}

/// Why there was nothing to compare with, in words a teacher can act on.
#[derive(Debug, Serialize)]
pub struct Unavailable {
    /// A stable code the page can branch on.
    code: &'static str,
    message: String,
}

impl Unavailable {
    pub fn no_repository() -> Self {
        Self {
            code: "noRepository",
            message: "This course has no linked repository to read solutions from.".into(),
        }
    }

    pub fn not_github() -> Self {
        Self {
            code: "notGithub",
            message: "The linked repository isn't on GitHub, so solutions can't be read from it."
                .into(),
        }
    }
}

impl From<FetchError> for Unavailable {
    fn from(e: FetchError) -> Self {
        let code = match e {
            FetchError::Denied => "denied",
            FetchError::RepoOrRefNotFound => "notFound",
            FetchError::RateLimited => "rateLimited",
            FetchError::TooLarge => "tooLarge",
            FetchError::NotText => "notText",
            FetchError::Unavailable => "unreachable",
        };
        Self {
            code,
            message: format!("Couldn't read the solution: {e}."),
        }
    }
}

/// How a student's file stands against the reference. Every state is its own
/// variant, so "identical" can't carry a diff and a diff can't be empty.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Comparison {
    /// The course has no solutions set up.
    Unconfigured,
    /// The solutions couldn't be read.
    Unavailable(Unavailable),
    /// The solutions have no file for this one.
    NoReference,
    /// The student's file was cut short, so comparing would show lines as
    /// missing that are only past the cut.
    Uncomparable,
    /// The same text as the solution.
    Identical,
    /// It differs; `removed` rows are the solution's, `added` rows the student's.
    Differs(Diff),
}

/// What the teacher's pane is given: the comparison, and which file it was made
/// with once that is known.
#[derive(Debug, Serialize)]
pub struct Solution {
    #[serde(flatten)]
    comparison: Comparison,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<Located>,
}

impl Solution {
    pub fn new(comparison: Comparison, file: Option<Located>) -> Self {
        Self { comparison, file }
    }

    /// Nothing was looked up, so there is no file to name.
    pub fn without_file(comparison: Comparison) -> Self {
        Self::new(comparison, None)
    }
}

/// Line endings and a final newline shouldn't make two files differ: they are
/// invisible to a reader, and a solution saved on another OS would otherwise
/// differ from every line of a correct answer.
fn normalize(text: &str) -> String {
    let mut s = text.replace("\r\n", "\n");
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// The differences between the reference and the buffer, as unified-diff hunks
/// with the reference as the old side. Empty when the two are the same.
pub fn hunks(reference: &str, buffer: &str) -> Vec<Hunk> {
    let (old, new) = (normalize(reference), normalize(buffer));
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(&old, &new);

    diff.grouped_ops(CONTEXT)
        .iter()
        .filter_map(|group| {
            let first = group.first()?;
            let lines = group
                .iter()
                .flat_map(|op| diff.iter_changes(op))
                .map(|change| {
                    let text = change.value().trim_end_matches('\n').to_string();
                    match change.tag() {
                        ChangeTag::Equal => DiffLine::Context(text),
                        ChangeTag::Delete => DiffLine::Removed(text),
                        ChangeTag::Insert => DiffLine::Added(text),
                    }
                })
                .collect();
            // Unified diffs number lines from 1; the ranges count from 0.
            Some(Hunk {
                old_start: u32::try_from(first.old_range().start + 1).ok()?,
                new_start: u32::try_from(first.new_range().start + 1).ok()?,
                lines,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shape(hunks: &[Hunk]) -> Vec<(u32, u32, Vec<String>)> {
        hunks
            .iter()
            .map(|h| {
                let lines = h
                    .lines
                    .iter()
                    .map(|l| match l {
                        DiffLine::Context(t) => format!(" {t}"),
                        DiffLine::Added(t) => format!("+{t}"),
                        DiffLine::Removed(t) => format!("-{t}"),
                    })
                    .collect();
                (h.old_start, h.new_start, lines)
            })
            .collect()
    }

    #[test]
    fn the_same_text_has_no_differences() {
        assert!(hunks("a\nb\nc\n", "a\nb\nc\n").is_empty());
        assert!(hunks("", "").is_empty());
    }

    #[test]
    fn line_endings_and_a_final_newline_are_not_differences() {
        assert!(hunks("a\r\nb\r\n", "a\nb\n").is_empty(), "CRLF vs LF");
        assert!(
            hunks("a\nb", "a\nb\n").is_empty(),
            "a missing final newline"
        );
        assert!(hunks("a\nb\n", "a\r\nb").is_empty(), "both at once");
    }

    #[test]
    fn a_changed_line_is_a_removed_and_an_added_row_in_context() {
        let reference = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let student = "a\nb\nc\nd\nE\nf\ng\nh\n";
        assert_eq!(
            shape(&hunks(reference, student)),
            [(
                2,
                2,
                vec![" b", " c", " d", "-e", "+E", " f", " g", " h"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            )]
        );
    }

    #[test]
    fn the_solution_is_the_old_side() {
        // A line the student lacks is `-` (in the solution), one they added `+`.
        let h = hunks("keep\nneeded\n", "keep\nextra\n");
        let rows = &shape(&h)[0].2;
        assert!(rows.contains(&"-needed".to_string()), "{rows:?}");
        assert!(rows.contains(&"+extra".to_string()), "{rows:?}");
    }

    #[test]
    fn a_difference_at_the_very_top_or_bottom_is_numbered_from_the_right_line() {
        let top = shape(&hunks("x\nb\nc\n", "b\nc\n"));
        assert_eq!(top[0].0, 1);
        assert_eq!(top[0].2[0], "-x");

        let bottom = shape(&hunks("a\nb\n", "a\nb\nnew\n"));
        assert_eq!((bottom[0].0, bottom[0].1), (1, 1));
        assert_eq!(bottom[0].2.last().unwrap(), "+new");
    }

    #[test]
    fn far_apart_differences_are_separate_hunks() {
        let reference: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let student = reference
            .replace("line 3\n", "LINE 3\n")
            .replace("line 28\n", "LINE 28\n");
        let h = hunks(&reference, &student);
        assert_eq!(h.len(), 2, "{:?}", shape(&h));
        assert_eq!(
            (h[0].old_start, h[1].old_start),
            (1, 25),
            "context of three lines each"
        );
    }

    #[test]
    fn a_student_with_nothing_yet_differs_from_every_line() {
        let h = hunks("a\nb\n", "");
        let rows = &shape(&h)[0].2;
        assert_eq!(rows, &["-a", "-b"]);
    }

    #[test]
    fn hunks_render_as_the_same_rows_as_a_commit_diff() {
        let diff = Diff::new(hunks("a\nb\nc\n", "a\nB\nc\n"), None);
        let v = serde_json::to_value(&diff).unwrap();
        assert_eq!(
            (v["added"].clone(), v["removed"].clone()),
            (json!(1), json!(1))
        );
        let rows = &v["hunks"][0]["rows"];
        assert_eq!(
            rows[1],
            json!({"sign": "-", "old": 2, "spans": [["", "b"]]})
        );
        assert_eq!(rows[2]["sign"], "+");
        assert_eq!(rows[2]["new"], 2);
    }

    #[test]
    fn each_state_says_what_it_is_and_carries_only_what_fits() {
        let j = |c: Comparison| serde_json::to_value(Solution::without_file(c)).unwrap();
        assert_eq!(
            j(Comparison::Unconfigured),
            json!({"state": "unconfigured"})
        );
        assert_eq!(j(Comparison::NoReference), json!({"state": "noReference"}));
        assert_eq!(
            j(Comparison::Uncomparable),
            json!({"state": "uncomparable"})
        );
        assert_eq!(j(Comparison::Identical), json!({"state": "identical"}));
        let unavailable = j(Comparison::Unavailable(FetchError::RateLimited.into()));
        assert_eq!(unavailable["state"], "unavailable");
        assert_eq!(unavailable["code"], "rateLimited");
        assert!(unavailable["message"]
            .as_str()
            .unwrap()
            .contains("rate limit"));
        let differs = j(Comparison::Differs(Diff::new(hunks("a\n", "b\n"), None)));
        assert_eq!(differs["state"], "differs");
        assert_eq!(
            (differs["added"].clone(), differs["removed"].clone()),
            (json!(1), json!(1))
        );
    }

    #[test]
    fn the_compared_file_is_named_when_one_was_looked_up() {
        let path = RepoPath::parse("answers/ex1/a.c").unwrap();
        let r = GitRef::parse("solutions").unwrap();
        let s = Solution::new(Comparison::Identical, Some(Located::new(&path, Some(&r))));
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            json!({"state": "identical", "file": {"path": "answers/ex1/a.c", "ref": "solutions"}})
        );
        let default_branch = Solution::new(Comparison::Identical, Some(Located::new(&path, None)));
        let v = serde_json::to_value(default_branch).unwrap();
        assert!(v["file"].get("ref").is_none(), "no ref set, none shown");
    }
}
