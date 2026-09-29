//! Repository-scoped GitHub App installation tokens for exercise seeding.
//!
//! Server-side seeding reads a linked repo's top-level folder names through the
//! GitHub contents API. With a single shared Personal Access Token (see
//! [`crate::repo`]) a teacher could link a course to *any* repo URL and, if the
//! token can read it, disclose that repo's folder names — a cross-repository
//! metadata leak the PAT allow-list only approximates.
//!
//! A configured GitHub App closes that gap at runtime: for each seeding request
//! it mints a short-lived installation access token scoped to exactly the
//! `owner/repo` being seeded, with read-only `contents`/`metadata` permission.
//! That token cannot read any other repository, so the authorization boundary is
//! enforced by GitHub rather than by our own list. If the app isn't installed on
//! the repo, minting fails and the caller falls back to the PAT (or public
//! seeding) — everything here stays best-effort.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};

use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::github_access::{api_request, ACCEPT_JSON};

/// A configured GitHub App: its id and the RSA private key that signs app JWTs.
#[derive(Clone)]
pub struct GithubApp {
    app_id: String,
    key: EncodingKey,
    tokens: Arc<TokenCache>,
}

/// An installation token and when GitHub stops honouring it.
struct Minted {
    token: String,
    expires_at: DateTime<Utc>,
}

/// How long before its expiry a token is no longer handed out, so a request
/// that starts with it does not finish after GitHub has stopped accepting it.
const EXPIRY_MARGIN_SECS: i64 = 60;
/// Installation tokens last an hour; used when GitHub's own expiry is unreadable.
const FALLBACK_LIFETIME_MINS: i64 = 30;

/// Installation tokens by repo, so one is minted per repo per hour rather than
/// once per read. Minting is two round trips plus an RSA signature, and a class
/// opening its dashboard asks for the same repo again and again.
///
/// Each repo has its own slot, held while a token is being minted: concurrent
/// requests for one repo wait for a single mint instead of each making their
/// own, and a slow mint for one repo never holds up another. A failed mint is
/// not remembered — the next request tries again.
#[derive(Default)]
struct TokenCache {
    slots: StdMutex<HashMap<RepoKey, Slot>>,
}

/// Lower-cased `(owner, repo)`.
type RepoKey = (String, String);
type Slot = Arc<Mutex<Option<Minted>>>;

impl TokenCache {
    async fn get_or_mint<F, Fut>(&self, owner: &str, repo: &str, mint: F) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Minted, String>>,
    {
        // GitHub treats owner and repo names case-insensitively.
        let key = (owner.to_ascii_lowercase(), repo.to_ascii_lowercase());
        let slot = self
            .slots
            .lock()
            .expect("the token map is never held across a panic")
            .entry(key)
            .or_default()
            .clone();
        let mut held = slot.lock().await;
        if let Some(minted) = held.as_ref() {
            if minted.expires_at - Duration::seconds(EXPIRY_MARGIN_SECS) > Utc::now() {
                return Ok(minted.token.clone());
            }
        }
        let minted = mint().await?;
        let token = minted.token.clone();
        *held = Some(minted);
        Ok(token)
    }
}

/// Claims for the app JWT GitHub requires to act as the app itself.
#[derive(Serialize)]
struct AppClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

#[derive(Deserialize)]
struct Installation {
    id: i64,
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
    /// RFC 3339, e.g. `2016-07-11T22:14:10Z`.
    expires_at: Option<String>,
}

impl GithubApp {
    /// Builds an app from its numeric id and the PEM-encoded RSA private key
    /// (the file contents GitHub gives you when you generate an app key).
    pub fn new(app_id: &str, private_key_pem: &str) -> Result<Self, String> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err("GitHub App id is empty".to_string());
        }
        let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|e| format!("invalid GitHub App private key: {e}"))?;
        Ok(Self {
            app_id: app_id.to_string(),
            key,
            tokens: Arc::default(),
        })
    }

    /// Signs a short-lived app JWT (RS256) used to authenticate as the app.
    fn app_jwt(&self) -> Result<String, String> {
        // GitHub caps the app JWT at 10 minutes and rejects a future `iat`, so
        // backdate slightly for clock skew and keep `exp` well under the cap.
        // The JWT only lives long enough to mint an installation token.
        let now = chrono::Utc::now().timestamp();
        let claims = AppClaims {
            iat: now - 60,
            exp: now + 9 * 60,
            iss: self.app_id.clone(),
        };
        encode(&Header::new(Algorithm::RS256), &claims, &self.key).map_err(|e| e.to_string())
    }

    /// An installation access token scoped to a single repository, with
    /// read-only `contents`/`metadata` permission — the cached one while it is
    /// good, otherwise a newly minted one. Returns `Err` (so the caller falls
    /// back to the PAT or public seeding) when the app isn't installed on the
    /// repo or GitHub rejects the request.
    pub async fn installation_token(&self, owner: &str, repo: &str) -> Result<String, String> {
        self.tokens
            .get_or_mint(owner, repo, || self.mint(owner, repo))
            .await
    }

    async fn mint(&self, owner: &str, repo: &str) -> Result<Minted, String> {
        let client = crate::github_access::client();
        let jwt = self.app_jwt()?;

        // Which installation covers this repo? (404 ⇒ app not installed there.)
        let install: Installation = api_request(
            &client,
            Method::GET,
            format!("https://api.github.com/repos/{owner}/{repo}/installation"),
            Some(&jwt),
            ACCEPT_JSON,
        )
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|_| format!("the GitHub App is not installed on {owner}/{repo}"))?
        .json()
        .await
        .map_err(|e| e.to_string())?;

        // Mint a token scoped to just this repo — it can read nothing else.
        let body = serde_json::json!({
            "repositories": [repo],
            "permissions": { "contents": "read", "metadata": "read" },
        });
        let minted: InstallationToken = api_request(
            &client,
            Method::POST,
            format!(
                "https://api.github.com/app/installations/{}/access_tokens",
                install.id
            ),
            Some(&jwt),
            ACCEPT_JSON,
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|_| "could not mint a GitHub App installation token".to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
        let expires_at = minted
            .expires_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| Utc::now() + Duration::minutes(FALLBACK_LIFETIME_MINS));
        Ok(Minted {
            token: minted.token,
            expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_app_id() {
        // The id is checked before the key, so any key value is fine here.
        assert!(GithubApp::new("  ", "unused").is_err());
    }

    #[test]
    fn rejects_bogus_private_key() {
        assert!(GithubApp::new("12345", "not a pem key").is_err());
    }

    fn minted(token: &str, lives_for: Duration) -> Minted {
        Minted {
            token: token.to_string(),
            expires_at: Utc::now() + lives_for,
        }
    }

    #[tokio::test]
    async fn a_good_token_is_minted_once() {
        let cache = TokenCache::default();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mint = || async {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(minted(&format!("t{n}"), Duration::minutes(59)))
        };
        assert_eq!(cache.get_or_mint("Org", "Repo", mint).await.unwrap(), "t0");
        // Same repo, however it is spelled: no second mint.
        assert_eq!(cache.get_or_mint("org", "repo", mint).await.unwrap(), "t0");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Another repo has its own token.
        assert_eq!(cache.get_or_mint("org", "other", mint).await.unwrap(), "t1");
    }

    #[tokio::test]
    async fn a_token_about_to_expire_is_replaced() {
        let cache = TokenCache::default();
        let first = cache
            .get_or_mint("o", "r", || async {
                Ok(minted("old", Duration::seconds(30)))
            })
            .await
            .unwrap();
        assert_eq!(first, "old");
        // Inside the margin, so it is not handed out again.
        let second = cache
            .get_or_mint("o", "r", || async {
                Ok(minted("new", Duration::minutes(59)))
            })
            .await
            .unwrap();
        assert_eq!(second, "new");
    }

    #[tokio::test]
    async fn a_failed_mint_is_not_remembered() {
        let cache = TokenCache::default();
        let failed = cache
            .get_or_mint("o", "r", || async { Err("not installed".to_string()) })
            .await;
        assert!(failed.is_err());
        let ok = cache
            .get_or_mint("o", "r", || async {
                Ok(minted("t", Duration::minutes(59)))
            })
            .await;
        assert_eq!(ok.unwrap(), "t");
    }

    #[tokio::test]
    async fn concurrent_requests_for_one_repo_share_one_mint() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = Arc::new(TokenCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let ask = |cache: Arc<TokenCache>, calls: Arc<AtomicUsize>| async move {
            cache
                .get_or_mint("o", "r", || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    Ok(minted("t", Duration::minutes(59)))
                })
                .await
                .unwrap()
        };
        let (a, b, c) = tokio::join!(
            ask(cache.clone(), calls.clone()),
            ask(cache.clone(), calls.clone()),
            ask(cache.clone(), calls.clone())
        );
        assert_eq!((a.as_str(), b.as_str(), c.as_str()), ("t", "t", "t"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
