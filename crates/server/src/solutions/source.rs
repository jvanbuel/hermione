//! Where a course keeps its reference solutions, and the path of one file in it.
//!
//! Both come from text somebody else typed: the branch and folder from a
//! teacher's settings form, the file path from a student's editor. They end up in
//! a request to the GitHub API, so each is parsed into a type that can only hold
//! a value that is safe there — a [`GitRef`] can't smuggle a `..` or a space, a
//! [`RepoPath`] can't climb out of its folder — and nothing downstream has to
//! check again.

use std::fmt;

/// Why a value was refused. The message is shown to the teacher who typed it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("a branch, tag or commit can't be empty")]
    EmptyRef,
    #[error("a branch, tag or commit is at most 100 characters")]
    RefTooLong,
    #[error("a branch, tag or commit may only use letters, digits and . _ / - (and can't start or end with / or ., contain .. or //, or start with -)")]
    BadRef,
    #[error("a path can't be empty")]
    EmptyPath,
    #[error("a path must be relative to the repository root, not start with /")]
    AbsolutePath,
    #[error("a path is at most 500 characters")]
    PathTooLong,
    #[error("a path can't contain empty, '.' or '..' segments, backslashes or control characters")]
    BadPath,
}

/// A branch, tag or commit in the linked repository.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitRef(String);

impl GitRef {
    pub fn parse(raw: &str) -> Result<Self, Invalid> {
        let s = raw.trim();
        if s.is_empty() {
            return Err(Invalid::EmptyRef);
        }
        if s.len() > 100 {
            return Err(Invalid::RefTooLong);
        }
        let charset = s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
        let shape = !s.starts_with(['-', '/', '.'])
            && !s.ends_with(['/', '.'])
            && !s.contains("..")
            && !s.contains("//")
            && !s.ends_with(".lock");
        if charset && shape {
            Ok(Self(s.to_string()))
        } else {
            Err(Invalid::BadRef)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A file or folder path inside the repository: relative, forward-slashed, with
/// no empty, `.` or `..` segments — so joining it under a folder can never leave
/// that folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RepoPath(String);

impl RepoPath {
    pub fn parse(raw: &str) -> Result<Self, Invalid> {
        // A trailing slash is how people write a folder; it isn't a segment.
        let s = raw.trim().trim_end_matches('/');
        if s.is_empty() {
            return Err(Invalid::EmptyPath);
        }
        if s.starts_with('/') {
            return Err(Invalid::AbsolutePath);
        }
        if s.len() > 500 {
            return Err(Invalid::PathTooLong);
        }
        let ok = s.split('/').all(|seg| {
            !matches!(seg, "" | "." | "..")
                && seg.len() <= 255
                && !seg.chars().any(|c| c == '\\' || c.is_control())
        });
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(Invalid::BadPath)
        }
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `self/tail`.
    pub fn join(&self, tail: &RepoPath) -> RepoPath {
        RepoPath(format!("{}/{}", self.0, tail.0))
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a course's reference solutions live in its linked repo: a ref, a
/// folder, or both. A course with neither has no solutions, and is represented
/// by the absence of a `SolutionsSource`, not by one holding two `None`s.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SolutionsSource {
    /// `None` ⇒ the repo's default branch.
    git_ref: Option<GitRef>,
    /// `None` ⇒ the repo root.
    dir: Option<RepoPath>,
}

impl SolutionsSource {
    /// From the two stored settings. Blank ones mean "not set"; both blank means
    /// the course has no solutions configured (`Ok(None)`).
    pub fn parse(git_ref: Option<&str>, dir: Option<&str>) -> Result<Option<Self>, Invalid> {
        let blank = |s: &&str| s.trim().is_empty();
        let git_ref = git_ref
            .filter(|s| !blank(s))
            .map(GitRef::parse)
            .transpose()?;
        let dir = dir.filter(|s| !blank(s)).map(RepoPath::parse).transpose()?;
        Ok((git_ref.is_some() || dir.is_some()).then_some(Self { git_ref, dir }))
    }

    /// From a course row. Settings that no longer parse (a hand-edited database)
    /// are treated as not configured rather than trusted.
    pub fn from_course(course: &hermione_entity::courses::Model) -> Option<Self> {
        Self::parse(
            course.solutions_ref.as_deref(),
            course.solutions_dir.as_deref(),
        )
        .ok()
        .flatten()
    }

    pub fn git_ref(&self) -> Option<&GitRef> {
        self.git_ref.as_ref()
    }

    #[cfg(test)]
    pub fn dir(&self) -> Option<&RepoPath> {
        self.dir.as_ref()
    }

    /// Where a student's file has its reference: the same path, under the folder.
    pub fn locate(&self, student_file: &RepoPath) -> RepoPath {
        match &self.dir {
            Some(dir) => dir.join(student_file),
            None => student_file.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_refs_are_accepted() {
        for ok in [
            "main",
            "solutions",
            "release/2026",
            "v1.2.3",
            "a-b_c",
            "0f1e2d3c4b",
        ] {
            assert_eq!(GitRef::parse(ok).unwrap().as_str(), ok, "{ok}");
        }
        assert_eq!(
            GitRef::parse("  solutions  ").unwrap().as_str(),
            "solutions"
        );
    }

    #[test]
    fn a_ref_cannot_carry_anything_that_steers_a_request() {
        for bad in [
            "a b", "a?b", "a#b", "a%2fb", "a..b", "../x", "/x", "x/", ".x", "x.", "-x", "a//b",
            "x.lock", "a\\b", "a\nb", "é", "a:b", "a@{b}", "a~1", "a^", "a*",
        ] {
            assert_eq!(GitRef::parse(bad), Err(Invalid::BadRef), "{bad:?}");
        }
        assert_eq!(GitRef::parse("  "), Err(Invalid::EmptyRef));
        assert_eq!(GitRef::parse(&"a".repeat(101)), Err(Invalid::RefTooLong));
    }

    #[test]
    fn ordinary_paths_are_accepted() {
        for ok in [
            "main.c",
            "ex02-strings/strings.c",
            "a/b/c.py",
            "my file.txt",
            "naïve/ü.rs",
            ".hermione.json",
        ] {
            assert_eq!(RepoPath::parse(ok).unwrap().as_str(), ok, "{ok}");
        }
        // A folder written with a trailing slash.
        assert_eq!(RepoPath::parse("solutions/").unwrap().as_str(), "solutions");
    }

    #[test]
    fn a_path_cannot_leave_its_folder_or_hide_a_separator() {
        for bad in [
            "../x", "a/../b", "a/./b", "a//b", "a\\b", "..", ".", "a/..", "a\u{0}b", "a\nb",
            "a/\tb",
        ] {
            assert_eq!(RepoPath::parse(bad), Err(Invalid::BadPath), "{bad:?}");
        }
        assert_eq!(RepoPath::parse("/etc/passwd"), Err(Invalid::AbsolutePath));
        assert_eq!(RepoPath::parse(""), Err(Invalid::EmptyPath));
        assert_eq!(RepoPath::parse("///"), Err(Invalid::EmptyPath));
        assert_eq!(
            RepoPath::parse(&"a/".repeat(300)),
            Err(Invalid::PathTooLong)
        );
        assert_eq!(RepoPath::parse(&"a".repeat(256)), Err(Invalid::BadPath));
    }

    #[test]
    fn percent_signs_and_query_characters_are_data_not_syntax() {
        // They are legal in a file name; what matters is that the request
        // builder encodes them (tested there), never that they slip through raw.
        assert!(RepoPath::parse("100%/a?b#c.txt").is_ok());
    }

    #[test]
    fn a_folder_and_a_file_join_without_escaping() {
        let dir = RepoPath::parse("solutions").unwrap();
        let file = RepoPath::parse("ex02-strings/strings.c").unwrap();
        assert_eq!(dir.join(&file).as_str(), "solutions/ex02-strings/strings.c");
    }

    #[test]
    fn a_source_needs_a_ref_or_a_folder() {
        assert_eq!(SolutionsSource::parse(None, None), Ok(None));
        assert_eq!(
            SolutionsSource::parse(Some(""), Some("  ")),
            Ok(None),
            "blank means unset"
        );
        let by_branch = SolutionsSource::parse(Some("solutions"), None)
            .unwrap()
            .unwrap();
        assert_eq!(by_branch.git_ref().unwrap().as_str(), "solutions");
        assert!(by_branch.dir().is_none());
        let by_folder = SolutionsSource::parse(None, Some("solutions/"))
            .unwrap()
            .unwrap();
        assert!(by_folder.git_ref().is_none());
        assert_eq!(by_folder.dir().unwrap().as_str(), "solutions");
    }

    #[test]
    fn a_bad_half_refuses_the_whole_setting() {
        assert_eq!(
            SolutionsSource::parse(Some("a b"), Some("ok")),
            Err(Invalid::BadRef)
        );
        assert_eq!(
            SolutionsSource::parse(Some("ok"), Some("../x")),
            Err(Invalid::BadPath)
        );
    }

    #[test]
    fn a_students_file_is_looked_up_under_the_folder() {
        let file = RepoPath::parse("ex01/list.c").unwrap();
        let rooted = SolutionsSource::parse(Some("solutions"), None)
            .unwrap()
            .unwrap();
        assert_eq!(rooted.locate(&file).as_str(), "ex01/list.c");
        let nested = SolutionsSource::parse(Some("main"), Some("solutions"))
            .unwrap()
            .unwrap();
        assert_eq!(nested.locate(&file).as_str(), "solutions/ex01/list.c");
    }
}
