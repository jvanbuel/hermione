//! Reference solutions: what the teacher considers a correct version of each
//! file, read from the course's linked repo and compared with a student's
//! buffer — on the server, so a solution never travels to a student's machine.

mod cache;
mod fetch;
mod source;

pub use cache::Solutions;
pub use fetch::{FetchError, GitHubRepo};
pub use source::{GitRef, Invalid, RepoPath, SolutionsSource};

#[cfg(test)]
pub use cache::tests::Fake;
