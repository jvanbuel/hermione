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
    /// `https://api.github.com/`; a test points it at a local server.
    base: reqwest::Url,
}

const API: &str = "https://api.github.com/";

impl GitHub {
    pub fn new(access: GitHubAccess) -> Self {
        Self::with_base(access, reqwest::Url::parse(API).expect("a constant URL"))
    }

    fn with_base(access: GitHubAccess, base: reqwest::Url) -> Self {
        // No redirects: GitHub redirects renamed and transferred repos, and
        // reqwest keeps the Authorization header on a same-host redirect, so
        // following one could send the token for one owner to another's repo.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a plain HTTPS client always builds");
        Self {
            access,
            client,
            base,
        }
    }

    /// `GET /repos/{owner}/{repo}/contents/{path}?ref={ref}`. Every piece goes in
    /// as a path segment or a query value, so the encoder — not a format string —
    /// decides what `?`, `#` and `%` in a file name mean.
    fn contents_url(
        &self,
        repo: &GitHubRepo,
        git_ref: Option<&GitRef>,
        path: &RepoPath,
    ) -> reqwest::Url {
        let mut url = self.base.clone();
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
    fn commit_url(&self, repo: &GitHubRepo, git_ref: Option<&GitRef>) -> reqwest::Url {
        let mut url = self.base.clone();
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

        let url = self.contents_url(repo, git_ref, path);
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
                    self.commit_url(repo, git_ref),
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

    fn gh() -> GitHub {
        GitHub::new(GitHubAccess::default())
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
        let url = gh().contents_url(&repo(), Some(&r), &path("ex02-strings/strings.c"));
        assert_eq!(
            url.as_str(),
            "https://api.github.com/repos/acme/cs101/contents/ex02-strings/strings.c?ref=solutions"
        );
        let default = gh().contents_url(&repo(), None, &path("a.c"));
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
        let url = gh().contents_url(&repo(), None, &nasty);
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
        let url = gh().contents_url(&repo(), Some(&r), &path("a.c"));
        assert_eq!(url.query(), Some("ref=release%2F2026"));
        assert_eq!(url.path(), "/repos/acme/cs101/contents/a.c");
    }

    #[test]
    fn the_probe_asks_about_the_ref_or_the_default_branch() {
        let r = GitRef::parse("release/2026").unwrap();
        assert_eq!(
            gh().commit_url(&repo(), Some(&r)).path(),
            "/repos/acme/cs101/commits/release/2026"
        );
        assert_eq!(
            gh().commit_url(&repo(), None).path(),
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

    // ---- the real fetcher, against a local stand-in for api.github.com ----

    use std::sync::{Arc, Mutex};

    use axum::{
        body::Body,
        extract::{Path, RawQuery, State},
        http::{header, HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
        Router,
    };

    /// What one request to the stand-in looked like.
    #[derive(Clone, Debug)]
    struct Seen {
        path: String,
        query: Option<String>,
        authorization: Option<String>,
        accept: Option<String>,
        user_agent: Option<String>,
    }

    type Log = Arc<Mutex<Vec<Seen>>>;

    fn record(log: &Log, path: String, query: Option<String>, h: &HeaderMap) {
        let get = |name: &str| {
            h.get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        log.lock().unwrap().push(Seen {
            path,
            query,
            authorization: get("authorization"),
            accept: get("accept"),
            user_agent: get("user-agent"),
        });
    }

    async fn contents(
        State(log): State<Log>,
        Path(path): Path<String>,
        RawQuery(query): RawQuery,
        headers: HeaderMap,
    ) -> Response {
        record(&log, format!("contents/{path}"), query, &headers);
        match path.as_str() {
            "ok.c" | "sub/nested.c" => (
                [(header::CONTENT_TYPE, "application/vnd.github.raw")],
                "int main() {}\n",
            )
                .into_response(),
            "dir" => (
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                "[]",
            )
                .into_response(),
            "big.c" => "x".repeat(MAX_BYTES + 1).into_response(),
            // No Content-Length: only reading it out with a cap can stop this.
            "stream.c" => {
                let chunk = || {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![
                        b'x';
                        64 * 1024
                    ]))
                };
                Body::from_stream(futures::stream::iter((0..8).map(move |_| chunk())))
                    .into_response()
            }
            "bin.dat" => vec![0xffu8, 0xfe, 0xfd].into_response(),
            "denied.c" => StatusCode::FORBIDDEN.into_response(),
            "limited.c" => {
                (StatusCode::FORBIDDEN, [("x-ratelimit-remaining", "0")]).into_response()
            }
            "boom.c" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            "moved.c" => (
                StatusCode::MOVED_PERMANENTLY,
                [(header::LOCATION, "/elsewhere")],
            )
                .into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn commits(
        State(log): State<Log>,
        Path(r#ref): Path<String>,
        RawQuery(query): RawQuery,
        headers: HeaderMap,
    ) -> Response {
        record(&log, format!("commits/{}", r#ref), query, &headers);
        match r#ref.as_str() {
            "solutions" | "release/2026" | "HEAD" => "0123abc".into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn elsewhere(State(log): State<Log>, headers: HeaderMap) -> Response {
        record(&log, "elsewhere".into(), None, &headers);
        "you should not be here".into_response()
    }

    /// A local server that answers like the parts of GitHub's API we use.
    async fn stand_in(access: GitHubAccess) -> (GitHub, Log) {
        let log: Log = Log::default();
        let app = Router::new()
            .route("/repos/acme/cs101/contents/{*path}", get(contents))
            .route("/repos/acme/cs101/commits/{*ref}", get(commits))
            .route("/elsewhere", get(elsewhere))
            .with_state(log.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (GitHub::with_base(access, base), log)
    }

    fn solutions_ref() -> GitRef {
        GitRef::parse("solutions").unwrap()
    }

    async fn read(
        gh: &GitHub,
        git_ref: Option<&GitRef>,
        file: &str,
    ) -> Result<Option<String>, FetchError> {
        gh.fetch(&repo(), git_ref, &path(file)).await
    }

    fn paths(log: &Log) -> Vec<String> {
        log.lock().unwrap().iter().map(|s| s.path.clone()).collect()
    }

    #[tokio::test]
    async fn a_file_is_read_with_the_headers_github_expects() {
        let (gh, log) = stand_in(GitHubAccess::default()).await;
        assert_eq!(
            read(&gh, Some(&solutions_ref()), "sub/nested.c").await,
            Ok(Some("int main() {}\n".into()))
        );
        let seen = log.lock().unwrap()[0].clone();
        assert_eq!(seen.path, "contents/sub/nested.c");
        assert_eq!(seen.query.as_deref(), Some("ref=solutions"));
        assert_eq!(
            seen.accept.as_deref(),
            Some("application/vnd.github.raw+json"),
            "the raw file, not JSON around it"
        );
        assert_eq!(
            seen.user_agent.as_deref(),
            Some("hermione"),
            "GitHub refuses requests without one"
        );
        assert_eq!(seen.authorization, None, "no credential, none sent");
    }

    #[tokio::test]
    async fn the_shared_token_goes_only_to_an_allow_listed_owner() {
        let allowed = GitHubAccess::new(Some("pat".into()), vec!["acme".into()], None);
        let (gh, log) = stand_in(allowed).await;
        read(&gh, None, "ok.c").await.unwrap();
        assert_eq!(
            log.lock().unwrap()[0].authorization.as_deref(),
            Some("Bearer pat")
        );

        let not_listed = GitHubAccess::new(Some("pat".into()), vec!["someone-else".into()], None);
        let (gh, log) = stand_in(not_listed).await;
        read(&gh, None, "ok.c").await.unwrap();
        assert_eq!(
            log.lock().unwrap()[0].authorization,
            None,
            "the token is never sent to an owner nobody allowed"
        );
    }

    #[tokio::test]
    async fn a_missing_file_and_a_missing_branch_are_told_apart() {
        let (gh, log) = stand_in(GitHubAccess::default()).await;
        // The branch exists, the file doesn't: there is simply no solution for it.
        assert_eq!(read(&gh, Some(&solutions_ref()), "gone.c").await, Ok(None));
        assert_eq!(
            paths(&log),
            ["contents/gone.c", "commits/solutions"],
            "one probe, to find out why"
        );
        // The branch doesn't exist: that is a mistake in the settings.
        let typo = GitRef::parse("solutoins").unwrap();
        assert_eq!(
            read(&gh, Some(&typo), "gone.c").await,
            Err(FetchError::RepoOrRefNotFound)
        );
        // With no ref set, the probe asks about the default branch.
        log.lock().unwrap().clear();
        assert_eq!(read(&gh, None, "gone.c").await, Ok(None));
        assert_eq!(paths(&log)[1], "commits/HEAD");
        // A branch name with a slash reaches GitHub as path segments.
        let nested = GitRef::parse("release/2026").unwrap();
        assert_eq!(read(&gh, Some(&nested), "gone.c").await, Ok(None));
    }

    #[tokio::test]
    async fn a_folder_is_not_a_file() {
        let (gh, _) = stand_in(GitHubAccess::default()).await;
        assert_eq!(
            read(&gh, None, "dir").await,
            Ok(None),
            "GitHub answers a folder with a JSON listing"
        );
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_whether_or_not_it_says_how_big_it_is() {
        let (gh, _) = stand_in(GitHubAccess::default()).await;
        assert_eq!(
            read(&gh, None, "big.c").await,
            Err(FetchError::TooLarge),
            "announced by Content-Length"
        );
        assert_eq!(
            read(&gh, None, "stream.c").await,
            Err(FetchError::TooLarge),
            "found out while reading"
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_text_is_refused() {
        let (gh, _) = stand_in(GitHubAccess::default()).await;
        assert_eq!(read(&gh, None, "bin.dat").await, Err(FetchError::NotText));
    }

    #[tokio::test]
    async fn refusals_read_as_what_they_are() {
        let (gh, _) = stand_in(GitHubAccess::default()).await;
        assert_eq!(read(&gh, None, "denied.c").await, Err(FetchError::Denied));
        assert_eq!(
            read(&gh, None, "limited.c").await,
            Err(FetchError::RateLimited)
        );
        assert_eq!(
            read(&gh, None, "boom.c").await,
            Err(FetchError::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_so_a_token_cannot_go_with_it() {
        let allowed = GitHubAccess::new(Some("pat".into()), vec!["acme".into()], None);
        let (gh, log) = stand_in(allowed).await;
        assert_eq!(
            read(&gh, None, "moved.c").await,
            Err(FetchError::Unavailable)
        );
        assert!(
            !paths(&log).contains(&"elsewhere".to_string()),
            "nothing was requested at the redirect target: {:?}",
            paths(&log)
        );
    }

    #[tokio::test]
    async fn a_server_that_is_not_there_is_unavailable_not_a_panic() {
        let gh = GitHub::with_base(
            GitHubAccess::default(),
            reqwest::Url::parse("http://127.0.0.1:1/").unwrap(),
        );
        assert_eq!(read(&gh, None, "ok.c").await, Err(FetchError::Unavailable));
    }
}
