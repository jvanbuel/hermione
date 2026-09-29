//! Reading one file from a GitHub repo at a given ref.
//!
//! Behind a trait so everything above it — the cache, the comparison, the
//! endpoint — is tested against an in-memory fake instead of the network.

use std::time::Duration;

use super::source::{GitRef, RepoPath};
use crate::github_access::GitHubAccess;

/// A file bigger than this isn't compared. It matches the cap on a student's
/// buffer (`snapshots::model`), so anything a buffer can be, a solution can be.
pub const MAX_BYTES: usize = 256 * 1024;

/// A repository on GitHub. Built only from a URL that parsed as one, so `owner`
/// and `name` are safe to put in a request path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitHubRepo {
    owner: String,
    name: String,
}

impl GitHubRepo {
    /// `None` for a URL that isn't a GitHub repo.
    pub fn parse(repo_url: &str) -> Option<Self> {
        crate::repo::parse_github(repo_url).map(|(owner, name)| Self { owner, name })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Why a file couldn't be read. Each says what the teacher can do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("Hermione isn't allowed to read that repository")]
    Denied,
    #[error("the repository, or the branch, tag or commit set for solutions, wasn't found")]
    RepoOrRefNotFound,
    #[error("GitHub's rate limit was reached; try again in a minute")]
    RateLimited,
    #[error("the solution file is larger than 256 KB")]
    TooLarge,
    #[error("the solution file isn't text")]
    NotText,
    #[error("GitHub couldn't be reached")]
    Unavailable,
}

/// Something that can read a file from a repo. `Ok(None)` means the repo and ref
/// are fine and the file simply isn't there.
#[tonic::async_trait]
pub trait Fetch: Send + Sync + 'static {
    async fn fetch(
        &self,
        repo: &GitHubRepo,
        git_ref: Option<&GitRef>,
        path: &RepoPath,
    ) -> Result<Option<String>, FetchError>;
}

/// Reads through the GitHub contents API, with whichever credential the server
/// may use for that repo (see [`GitHubAccess`]).
pub struct GitHub {
    access: GitHubAccess,
    client: reqwest::Client,
}

const API: &str = "https://api.github.com/";

impl GitHub {
    pub fn new(access: GitHubAccess) -> Self {
        // No redirects: GitHub redirects renamed and transferred repos, and
        // reqwest keeps the Authorization header on a same-host redirect, so
        // following one could send the token for one owner to another's repo.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a plain HTTPS client always builds");
        Self { access, client }
    }

    /// `GET /repos/{owner}/{repo}/contents/{path}?ref={ref}`. Every piece goes in
    /// as a path segment or a query value, so the encoder — not a format string —
    /// decides what `?`, `#` and `%` in a file name mean.
    fn contents_url(repo: &GitHubRepo, git_ref: Option<&GitRef>, path: &RepoPath) -> reqwest::Url {
        let mut url = reqwest::Url::parse(API).expect("a constant URL");
        url.path_segments_mut()
            .expect("an https URL has path segments")
            .extend(["repos", repo.owner(), repo.name(), "contents"])
            .extend(path.segments());
        if let Some(r) = git_ref {
            url.query_pairs_mut().append_pair("ref", r.as_str());
        }
        url
    }

    /// `GET /repos/{owner}/{repo}/commits/{ref}` — does the ref (or, with none,
    /// the default branch) exist and can we read it?
    fn commit_url(repo: &GitHubRepo, git_ref: Option<&GitRef>) -> reqwest::Url {
        let mut url = reqwest::Url::parse(API).expect("a constant URL");
        let mut segments = url
            .path_segments_mut()
            .expect("an https URL has path segments");
        segments.extend(["repos", repo.owner(), repo.name(), "commits"]);
        match git_ref {
            Some(r) => segments.extend(r.as_str().split('/')),
            None => segments.push("HEAD"),
        };
        drop(segments);
        url
    }

    fn get(&self, url: reqwest::Url, token: Option<&str>, accept: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .get(url)
            .header("User-Agent", "hermione")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Accept", accept);
        match token {
            Some(t) => request.bearer_auth(t),
            None => request,
        }
    }
}

/// How a non-success status reads to the teacher.
fn refusal(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> FetchError {
    let out_of_requests = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v == "0");
    match status.as_u16() {
        429 => FetchError::RateLimited,
        403 if out_of_requests => FetchError::RateLimited,
        401 | 403 => FetchError::Denied,
        404 | 422 => FetchError::RepoOrRefNotFound,
        _ => FetchError::Unavailable,
    }
}

#[tonic::async_trait]
impl Fetch for GitHub {
    async fn fetch(
        &self,
        repo: &GitHubRepo,
        git_ref: Option<&GitRef>,
        path: &RepoPath,
    ) -> Result<Option<String>, FetchError> {
        let credential = self.access.credential_for(repo.owner(), repo.name()).await;
        let token = credential.token();

        let url = Self::contents_url(repo, git_ref, path);
        let mut response = self
            .get(url, token, "application/vnd.github.raw+json")
            .send()
            .await
            .map_err(|_| FetchError::Unavailable)?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            // GitHub answers 404 for a missing file, a missing ref and a repo it
            // won't show us alike. Telling them apart is worth one more request,
            // and the caller caches the answer.
            let probe = self
                .get(
                    Self::commit_url(repo, git_ref),
                    token,
                    "application/vnd.github.sha",
                )
                .send()
                .await
                .map_err(|_| FetchError::Unavailable)?;
            return if probe.status().is_success() {
                Ok(None)
            } else {
                Err(refusal(probe.status(), probe.headers()))
            };
        }
        if !status.is_success() {
            return Err(refusal(status, response.headers()));
        }

        // A folder comes back as a JSON listing, not a file.
        let is_listing = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("application/json"));
        if is_listing {
            return Ok(None);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(FetchError::TooLarge);
        }

        // Read at most the cap, so a file that doesn't announce its size still
        // can't fill memory.
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| FetchError::Unavailable)?
        {
            if body.len() + chunk.len() > MAX_BYTES {
                return Err(FetchError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body)
            .map(Some)
            .map_err(|_| FetchError::NotText)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> GitHubRepo {
        GitHubRepo::parse("https://github.com/acme/cs101").unwrap()
    }

    fn path(s: &str) -> RepoPath {
        RepoPath::parse(s).unwrap()
    }

    #[test]
    fn only_github_urls_are_repos() {
        assert_eq!(repo().to_string(), "acme/cs101");
        assert!(GitHubRepo::parse("git@github.com:acme/cs101.git").is_some());
        assert!(GitHubRepo::parse("https://gitlab.com/acme/cs101").is_none());
        assert!(GitHubRepo::parse("https://github.com/acme/cs101?x=1").is_none());
    }

    #[test]
    fn a_files_url_names_the_repo_the_path_and_the_ref() {
        let r = GitRef::parse("solutions").unwrap();
        let url = GitHub::contents_url(&repo(), Some(&r), &path("ex02-strings/strings.c"));
        assert_eq!(
            url.as_str(),
            "https://api.github.com/repos/acme/cs101/contents/ex02-strings/strings.c?ref=solutions"
        );
        let default = GitHub::contents_url(&repo(), None, &path("a.c"));
        assert_eq!(
            default.query(),
            None,
            "no ref: GitHub uses the default branch"
        );
    }

    #[test]
    fn characters_that_mean_something_in_a_url_stay_inside_their_segment() {
        // A student's editor chose this path. Whatever it contains, the request
        // must still be for a file under this repo, with no query but our own.
        let nasty = path("a b/100%/x?y=z#frag/é.c");
        let url = GitHub::contents_url(&repo(), None, &nasty);
        assert_eq!(url.host_str(), Some("api.github.com"));
        assert_eq!(url.query(), None, "a ? in a name is not a query");
        assert_eq!(url.fragment(), None, "a # in a name is not a fragment");
        let segments: Vec<_> = url.path_segments().unwrap().collect();
        assert_eq!(
            &segments[..5],
            ["repos", "acme", "cs101", "contents", "a%20b"]
        );
        assert_eq!(
            segments.len(),
            4 + 4,
            "one segment per path segment: {segments:?}"
        );
        assert!(segments[5].contains("%25"), "% is encoded: {segments:?}");
        assert!(
            segments[6].contains("%3F") && segments[6].contains("%23"),
            "{segments:?}"
        );
    }

    #[test]
    fn a_ref_with_slashes_or_odd_characters_stays_in_the_query() {
        let r = GitRef::parse("release/2026").unwrap();
        let url = GitHub::contents_url(&repo(), Some(&r), &path("a.c"));
        assert_eq!(url.query(), Some("ref=release%2F2026"));
        assert_eq!(url.path(), "/repos/acme/cs101/contents/a.c");
    }

    #[test]
    fn the_probe_asks_about_the_ref_or_the_default_branch() {
        let r = GitRef::parse("release/2026").unwrap();
        assert_eq!(
            GitHub::commit_url(&repo(), Some(&r)).path(),
            "/repos/acme/cs101/commits/release/2026"
        );
        assert_eq!(
            GitHub::commit_url(&repo(), None).path(),
            "/repos/acme/cs101/commits/HEAD"
        );
    }

    #[test]
    fn statuses_read_as_what_a_teacher_can_act_on() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let none = HeaderMap::new();
        let status = |c: u16| reqwest::StatusCode::from_u16(c).unwrap();
        assert_eq!(refusal(status(404), &none), FetchError::RepoOrRefNotFound);
        assert_eq!(refusal(status(422), &none), FetchError::RepoOrRefNotFound);
        assert_eq!(refusal(status(401), &none), FetchError::Denied);
        assert_eq!(refusal(status(403), &none), FetchError::Denied);
        assert_eq!(refusal(status(429), &none), FetchError::RateLimited);
        assert_eq!(refusal(status(502), &none), FetchError::Unavailable);
        let mut spent = HeaderMap::new();
        spent.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        assert_eq!(
            refusal(status(403), &spent),
            FetchError::RateLimited,
            "a 403 with no requests left"
        );
    }
}
