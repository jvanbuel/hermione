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

use std::time::Duration;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

/// A configured GitHub App: its id and the RSA private key that signs app JWTs.
#[derive(Clone)]
pub struct GithubApp {
    app_id: String,
    key: EncodingKey,
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

    /// Mints an installation access token scoped to a single repository, with
    /// read-only `contents`/`metadata` permission. Returns `Err` (so the caller
    /// falls back to the PAT or public seeding) when the app isn't installed on
    /// the repo or GitHub rejects the request.
    pub async fn installation_token(&self, owner: &str, repo: &str) -> Result<String, String> {
        // Don't follow redirects: GitHub 301-redirects renamed/transferred repos
        // and reqwest keeps the Authorization header on same-host redirects, so
        // following one could aim the app JWT at a different owner. A redirecting
        // repo simply isn't seeded via the app.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let jwt = self.app_jwt()?;

        // Which installation covers this repo? (404 ⇒ app not installed there.)
        let install: Installation = client
            .get(format!(
                "https://api.github.com/repos/{owner}/{repo}/installation"
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("User-Agent", "hermione")
            .header("Accept", "application/vnd.github+json")
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
        let minted: InstallationToken = client
            .post(format!(
                "https://api.github.com/app/installations/{}/access_tokens",
                install.id
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("User-Agent", "hermione")
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|_| "could not mint a GitHub App installation token".to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(minted.token)
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
}
