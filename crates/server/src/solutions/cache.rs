//! Reference solutions, read once and remembered for a while.
//!
//! A teacher with a student's Solution view open polls about once a second, and
//! every poll needs the same file. Without a cache that is a GitHub request (or,
//! with a GitHub App, three) per second per pane, which would spend the rate
//! limit in minutes. Solutions change rarely, so a few minutes' staleness costs
//! nothing, while a mistake the teacher is about to fix — a wrong branch, a file
//! not pushed yet — is only remembered briefly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::fetch::{Fetch, FetchError, GitHub, GitHubRepo};
use super::source::{GitRef, RepoPath, SolutionsSource};
use crate::github_access::GitHubAccess;

/// How long a file that was read is believed.
const FOUND_TTL: Duration = Duration::from_secs(300);
/// How long "there is no such file" is believed: the teacher may be about to
/// push it.
const MISSING_TTL: Duration = Duration::from_secs(60);
/// How long a failure is believed. Short, but not zero: a pane that polls every
/// second must not turn a GitHub outage into a request storm.
const FAILED_TTL: Duration = Duration::from_secs(20);
/// Entries kept before expired ones are swept out.
const SWEEP_AT: usize = 512;

/// What was found: the solution's text, or that there is none for this file.
pub type Outcome = Result<Option<Arc<str>>, FetchError>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    repo: GitHubRepo,
    git_ref: Option<GitRef>,
    path: RepoPath,
}

struct Entry {
    at: Instant,
    outcome: Outcome,
}

impl Entry {
    fn fresh(&self, now: Instant) -> bool {
        let ttl = match &self.outcome {
            Ok(Some(_)) => FOUND_TTL,
            Ok(None) => MISSING_TTL,
            Err(_) => FAILED_TTL,
        };
        now.duration_since(self.at) < ttl
    }
}

/// One file's slot. Readers of the same file queue on it, so a burst of polls
/// while it is being fetched makes one request, not one each.
type Slot = Arc<tokio::sync::Mutex<Option<Entry>>>;

#[derive(Clone)]
pub struct Solutions {
    fetch: Arc<dyn Fetch>,
    slots: Arc<Mutex<HashMap<Key, Slot>>>,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Solutions {
    pub fn new(fetch: Arc<dyn Fetch>) -> Self {
        Self::with_clock(fetch, Arc::new(Instant::now))
    }

    /// Reads from GitHub with whatever credentials the server has.
    pub fn github(access: GitHubAccess) -> Self {
        Self::new(Arc::new(GitHub::new(access)))
    }

    /// For tests, which move time themselves.
    pub fn with_clock(fetch: Arc<dyn Fetch>, now: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            fetch,
            slots: Arc::default(),
            now,
        }
    }

    /// The reference for `student_file`: looked up under the course's solutions
    /// folder, at its ref, in its repo.
    pub async fn read(
        &self,
        repo: &GitHubRepo,
        source: &SolutionsSource,
        student_file: &RepoPath,
    ) -> Outcome {
        let key = Key {
            repo: repo.clone(),
            git_ref: source.git_ref().cloned(),
            path: source.locate(student_file),
        };
        let slot = self.slot(&key);
        let mut entry = slot.lock().await;
        if let Some(e) = entry.as_ref().filter(|e| e.fresh((self.now)())) {
            return e.outcome.clone();
        }
        let outcome = self
            .fetch
            .fetch(&key.repo, key.git_ref.as_ref(), &key.path)
            .await
            .map(|found| found.map(Arc::from));
        *entry = Some(Entry {
            at: (self.now)(),
            outcome: outcome.clone(),
        });
        outcome
    }

    fn slot(&self, key: &Key) -> Slot {
        // Nothing here awaits or leaves the map half-updated, so a poisoned lock
        // still guards consistent data.
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.len() >= SWEEP_AT {
            let now = (self.now)();
            // A slot someone is waiting on is in use: keep it.
            slots.retain(|_, s| match s.try_lock() {
                Ok(e) => e.as_ref().is_some_and(|e| e.fresh(now)),
                Err(_) => true,
            });
        }
        Arc::clone(slots.entry(key.clone()).or_default())
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A repo held in memory, counting how often it is asked.
    #[derive(Default)]
    pub struct Fake {
        pub files: Mutex<HashMap<String, Result<Option<String>, FetchError>>>,
        pub calls: AtomicUsize,
    }

    impl Fake {
        pub fn with(self, path: &str, file: Result<Option<String>, FetchError>) -> Self {
            self.files.lock().unwrap().insert(path.to_string(), file);
            self
        }
    }

    #[tonic::async_trait]
    impl Fetch for Fake {
        async fn fetch(
            &self,
            _repo: &GitHubRepo,
            _git_ref: Option<&GitRef>,
            path: &RepoPath,
        ) -> Result<Option<String>, FetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Give concurrent readers time to pile up behind this one.
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.files
                .lock()
                .unwrap()
                .get(path.as_str())
                .cloned()
                .unwrap_or(Ok(None))
        }
    }

    fn repo() -> GitHubRepo {
        GitHubRepo::parse("https://github.com/acme/cs101").unwrap()
    }

    fn source() -> SolutionsSource {
        SolutionsSource::parse(Some("solutions"), Some("answers"))
            .unwrap()
            .unwrap()
    }

    fn path(s: &str) -> RepoPath {
        RepoPath::parse(s).unwrap()
    }

    /// A clock the test moves.
    fn clock() -> (Arc<Mutex<Instant>>, Arc<dyn Fn() -> Instant + Send + Sync>) {
        let t = Arc::new(Mutex::new(Instant::now()));
        let read = Arc::clone(&t);
        (t, Arc::new(move || *read.lock().unwrap()))
    }

    fn cache(fake: Fake) -> (Arc<Fake>, Solutions, Arc<Mutex<Instant>>) {
        let fake = Arc::new(fake);
        let (t, now) = clock();
        (fake.clone(), Solutions::with_clock(fake, now), t)
    }

    fn text(o: Outcome) -> Option<String> {
        o.unwrap().map(|t| t.to_string())
    }

    #[tokio::test]
    async fn a_file_is_looked_up_under_the_solutions_folder() {
        let (fake, solutions, _) =
            cache(Fake::default().with("answers/ex1/a.c", Ok(Some("int x;".into()))));
        let got = solutions.read(&repo(), &source(), &path("ex1/a.c")).await;
        assert_eq!(text(got).as_deref(), Some("int x;"));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn asking_again_within_the_lifetime_does_not_ask_github_again() {
        let (fake, solutions, t) = cache(Fake::default().with("answers/a.c", Ok(Some("x".into()))));
        for _ in 0..5 {
            solutions
                .read(&repo(), &source(), &path("a.c"))
                .await
                .unwrap();
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        *t.lock().unwrap() += FOUND_TTL + Duration::from_secs(1);
        solutions
            .read(&repo(), &source(), &path("a.c"))
            .await
            .unwrap();
        assert_eq!(
            fake.calls.load(Ordering::SeqCst),
            2,
            "a stale entry is read again"
        );
    }

    #[tokio::test]
    async fn a_missing_file_and_a_failure_are_remembered_more_briefly() {
        let fake = Fake::default()
            .with("answers/gone.c", Ok(None))
            .with("answers/bad.c", Err(FetchError::RateLimited));
        let (fake, solutions, t) = cache(fake);
        for name in ["gone.c", "bad.c"] {
            solutions.read(&repo(), &source(), &path(name)).await.ok();
            solutions.read(&repo(), &source(), &path(name)).await.ok();
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2, "each asked once");

        *t.lock().unwrap() += FAILED_TTL + Duration::from_secs(1);
        solutions
            .read(&repo(), &source(), &path("bad.c"))
            .await
            .ok();
        solutions
            .read(&repo(), &source(), &path("gone.c"))
            .await
            .ok();
        assert_eq!(
            fake.calls.load(Ordering::SeqCst),
            3,
            "a failure is retried sooner than a missing file"
        );

        *t.lock().unwrap() += MISSING_TTL;
        solutions
            .read(&repo(), &source(), &path("gone.c"))
            .await
            .ok();
        assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_failure_is_reported_as_what_it_was() {
        let (_, solutions, _) = cache(Fake::default().with("answers/a.c", Err(FetchError::Denied)));
        assert_eq!(
            solutions
                .read(&repo(), &source(), &path("a.c"))
                .await
                .unwrap_err(),
            FetchError::Denied
        );
    }

    #[tokio::test]
    async fn readers_of_the_same_file_share_one_request() {
        let (fake, solutions, _) = cache(Fake::default().with("answers/a.c", Ok(Some("x".into()))));
        let reads = (0..8).map(|_| {
            let (s, src) = (solutions.clone(), source());
            async move { s.read(&repo(), &src, &path("a.c")).await }
        });
        let all = futures::future::join_all(reads).await;
        assert!(all.iter().all(|o| text(o.clone()).as_deref() == Some("x")));
        assert_eq!(
            fake.calls.load(Ordering::SeqCst),
            1,
            "eight polls, one fetch"
        );
    }

    #[tokio::test]
    async fn different_files_refs_and_folders_are_kept_apart() {
        let fake = Fake::default()
            .with("answers/a.c", Ok(Some("A".into())))
            .with("answers/b.c", Ok(Some("B".into())))
            .with("a.c", Ok(Some("root".into())));
        let (fake, solutions, _) = cache(fake);
        assert_eq!(
            text(solutions.read(&repo(), &source(), &path("a.c")).await).as_deref(),
            Some("A")
        );
        assert_eq!(
            text(solutions.read(&repo(), &source(), &path("b.c")).await).as_deref(),
            Some("B")
        );
        let rooted = SolutionsSource::parse(Some("solutions"), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            text(solutions.read(&repo(), &rooted, &path("a.c")).await).as_deref(),
            Some("root")
        );
        let other_ref = SolutionsSource::parse(Some("v2"), Some("answers"))
            .unwrap()
            .unwrap();
        solutions
            .read(&repo(), &other_ref, &path("a.c"))
            .await
            .unwrap();
        assert_eq!(
            fake.calls.load(Ordering::SeqCst),
            4,
            "a different ref is a different file"
        );
    }

    #[tokio::test]
    async fn the_cache_is_bounded() {
        let (_, solutions, t) = cache(Fake::default());
        for i in 0..SWEEP_AT {
            solutions
                .read(&repo(), &source(), &path(&format!("f{i}.c")))
                .await
                .unwrap();
        }
        *t.lock().unwrap() += FOUND_TTL + MISSING_TTL;
        solutions
            .read(&repo(), &source(), &path("one-more.c"))
            .await
            .unwrap();
        let held = solutions.slots.lock().unwrap().len();
        assert!(held < SWEEP_AT, "expired entries were swept: {held}");
    }
}
