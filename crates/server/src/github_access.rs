//! Which GitHub credential, if any, the server may use to read a given repo.
//!
//! Seeding a course's exercises, drawing its repo tree and reading its reference
//! solutions all read the linked repo through the GitHub API, and all must make
//! the same decision — one place, so it can't drift between them:
//!
//! 1. A GitHub App installation token, scoped by GitHub to exactly this repo. It
//!    can read nothing else, so a teacher can't point a course at an unrelated
//!    private repo and read it.
//! 2. Otherwise the shared personal access token, but **only** for allow-listed
//!    owners, for the same reason.
//! 3. Otherwise nothing: public repos still work.

use crate::github_app::GithubApp;
use crate::repo::owner_allowed;

/// The HTTP client every read of api.github.com goes through.
///
/// It never follows redirects: GitHub 301-redirects renamed and transferred
/// repos, and reqwest keeps the `Authorization` header on a same-host redirect,
/// so following one could send the credential for one owner to another's repo.
/// A repo that redirects simply isn't read. Kept in one place so no caller can
/// forget it.
///
/// One client is built and shared: a `Client` is a handle to a connection pool,
/// so cloning it is cheap and keeps connections to GitHub alive between reads.
pub fn client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a plain HTTPS client always builds")
        })
        .clone()
}

/// The GitHub credentials the server was configured with.
#[derive(Clone, Default)]
pub struct GitHubAccess {
    /// A personal access token. Only ever sent to allow-listed owners.
    token: Option<String>,
    allowed_owners: Vec<String>,
    app: Option<GithubApp>,
}

/// What to send to GitHub when reading one repo.
pub struct Credential {
    token: Option<String>,
    /// A personal token exists but this owner isn't allow-listed and no app token
    /// covered the repo — so a failure reads as "not allowed", not "no token".
    withheld: bool,
}

impl Credential {
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Whether the shared token was deliberately not sent (see the struct docs).
    pub fn withheld(&self) -> bool {
        self.withheld
    }
}

impl GitHubAccess {
    pub fn new(token: Option<String>, allowed_owners: Vec<String>, app: Option<GithubApp>) -> Self {
        Self {
            token,
            allowed_owners,
            app,
        }
    }

    /// The credential to use for `owner/repo`, following the ladder above.
    pub async fn credential_for(&self, owner: &str, repo: &str) -> Credential {
        let app_token = match &self.app {
            Some(app) => match app.installation_token(owner, repo).await {
                Ok(token) => Some(token),
                Err(e) => {
                    tracing::debug!("no GitHub App token for {owner}/{repo}: {e}");
                    None
                }
            },
            None => None,
        };
        let allowed = owner_allowed(owner, &self.allowed_owners);
        let shared = self.token.as_deref().filter(|_| allowed);
        let withheld = app_token.is_none() && self.token.is_some() && !allowed;
        Credential {
            token: app_token.or_else(|| shared.map(str::to_string)),
            withheld,
        }
    }
}
