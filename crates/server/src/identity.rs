//! Verified student identity.
//!
//! Students authenticate with an IdP (GitHub, Google, or any OIDC provider).
//! The backend verifies the IdP credential and issues a short-lived, signed
//! *Hermione identity token* that the agents cache and present with each ingest,
//! so the `student` is server-trusted rather than self-asserted.

use jsonwebtoken::{
    decode, decode_header, encode, jwk::JwkSet, Algorithm, DecodingKey, EncodingKey, Header,
    Validation,
};
use serde::{Deserialize, Serialize};

/// Lifetime of an issued Hermione identity token.
pub const TOKEN_TTL_SECS: i64 = 8 * 60 * 60;

#[derive(Clone, Copy, Deserialize, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Standards-compliant OIDC: verify the ID token via discovery + JWKS.
    #[default]
    Oidc,
    /// GitHub OAuth (not OIDC): verify the access token via the GitHub API.
    Github,
}

/// A configured identity provider (from `HERMIONE_OIDC_PROVIDERS`, a JSON array).
#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub issuer: Option<String>,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Option<String>,
}

#[derive(Clone)]
pub struct Identity {
    providers: Vec<Provider>,
    /// HS256 signing secret for Hermione identity tokens. `None` disables auth.
    secret: Option<String>,
    http: reqwest::Client,
}

#[derive(Serialize, Deserialize)]
struct HermioneClaims {
    sub: String,
    exp: usize,
}

#[derive(Deserialize)]
struct OidcClaims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
    token_endpoint: Option<String>,
    device_authorization_endpoint: Option<String>,
}

/// Device-flow start response handed back to the agent.
#[derive(Deserialize, Serialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default)]
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

/// Result of polling a device-flow authorization.
pub enum DevicePoll {
    Pending,
    Done(String),
}

impl Identity {
    pub fn from_env() -> Self {
        let providers = std::env::var("HERMIONE_OIDC_PROVIDERS")
            .ok()
            .and_then(|s| match serde_json::from_str::<Vec<Provider>>(&s) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!("HERMIONE_OIDC_PROVIDERS is not valid JSON: {e}");
                    None
                }
            })
            .unwrap_or_default();
        let secret = std::env::var("HERMIONE_IDENTITY_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
        Self {
            providers,
            secret,
            http: reqwest::Client::new(),
        }
    }

    /// An identity-disabled instance (for tests / local dev without OIDC).
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            providers: Vec::new(),
            secret: None,
            http: reqwest::Client::new(),
        }
    }

    /// Builds an enforced instance for tests (a secret + providers, no live IdP).
    #[cfg(test)]
    pub fn for_test(secret: &str, providers: Vec<Provider>) -> Self {
        Self {
            providers,
            secret: Some(secret.to_string()),
            http: reqwest::Client::new(),
        }
    }

    /// Verified identity is enforced once a signing secret and a provider exist.
    pub fn enforced(&self) -> bool {
        self.secret.is_some() && !self.providers.is_empty()
    }

    fn provider(&self, name: &str) -> Result<&Provider, String> {
        self.providers
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("unknown provider: {name}"))
    }

    // --- Hermione identity token (HS256) ------------------------------------

    pub fn issue(&self, student: &str) -> Option<String> {
        let secret = self.secret.as_ref()?;
        let claims = HermioneClaims {
            sub: student.to_string(),
            exp: (now() + TOKEN_TTL_SECS) as usize,
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .ok()
    }

    /// Returns the verified student id from a Hermione identity token.
    pub fn verify(&self, token: &str) -> Option<String> {
        let secret = self.secret.as_ref()?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        decode::<HermioneClaims>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )
        .ok()
        .map(|d| d.claims.sub)
    }

    // --- IdP verification ---------------------------------------------------

    /// Verifies an IdP credential and returns a namespaced student id
    /// (e.g. "github:alice", "google:alice@x.edu").
    pub async fn verify_idp(&self, provider_name: &str, token: &str) -> Result<String, String> {
        let provider = self.provider(provider_name)?;
        match provider.kind {
            ProviderKind::Github => {
                let login = self.github_login(token).await?;
                Ok(format!("{}:{}", provider.name, login))
            }
            ProviderKind::Oidc => {
                let issuer = provider
                    .issuer
                    .as_deref()
                    .ok_or("oidc provider needs issuer")?;
                let email = self.verify_oidc(issuer, &provider.client_id, token).await?;
                Ok(format!("{}:{}", provider.name, email))
            }
        }
    }

    async fn github_login(&self, access_token: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct GithubUser {
            login: String,
        }
        let user: GithubUser = self
            .http
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", "hermione")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|_| "github rejected the token".to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(user.login)
    }

    async fn discovery(&self, issuer: &str) -> Result<Discovery, String> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        self.http
            .get(url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<Discovery>()
            .await
            .map_err(|e| e.to_string())
    }

    async fn verify_oidc(
        &self,
        issuer: &str,
        client_id: &str,
        id_token: &str,
    ) -> Result<String, String> {
        let discovery = self.discovery(issuer).await?;
        let jwks: JwkSet = self
            .http
            .get(&discovery.jwks_uri)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let claims = verify_id_token(id_token, &jwks, issuer, client_id)?;
        Ok(claims.email.unwrap_or(claims.sub))
    }

    // --- Device flow (for the CLI recorder) ---------------------------------

    pub async fn device_start(&self, provider_name: &str) -> Result<DeviceStart, String> {
        let provider = self.provider(provider_name)?;
        let (endpoint, default_scope) = match provider.kind {
            ProviderKind::Github => (
                "https://github.com/login/device/code".to_string(),
                "read:user".to_string(),
            ),
            ProviderKind::Oidc => {
                let issuer = provider
                    .issuer
                    .as_deref()
                    .ok_or("oidc provider needs issuer")?;
                let endpoint = self
                    .discovery(issuer)
                    .await?
                    .device_authorization_endpoint
                    .ok_or("provider has no device endpoint")?;
                (endpoint, "openid email".to_string())
            }
        };
        let scope = provider.scopes.clone().unwrap_or(default_scope);
        self.http
            .post(endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", provider.client_id.as_str()),
                ("scope", &scope),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<DeviceStart>()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn device_poll(
        &self,
        provider_name: &str,
        device_code: &str,
    ) -> Result<DevicePoll, String> {
        let provider = self.provider(provider_name)?;
        let token_endpoint = match provider.kind {
            ProviderKind::Github => "https://github.com/login/oauth/access_token".to_string(),
            ProviderKind::Oidc => {
                let issuer = provider
                    .issuer
                    .as_deref()
                    .ok_or("oidc provider needs issuer")?;
                self.discovery(issuer)
                    .await?
                    .token_endpoint
                    .ok_or("provider has no token endpoint")?
            }
        };

        let grant = "urn:ietf:params:oauth:grant-type:device_code";
        let mut form: Vec<(&str, &str)> = vec![
            ("client_id", provider.client_id.as_str()),
            ("device_code", device_code),
            ("grant_type", grant),
        ];
        if let Some(secret) = &provider.client_secret {
            form.push(("client_secret", secret));
        }

        let body: serde_json::Value = self
            .http
            .post(token_endpoint)
            .header("Accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
            return match err {
                "authorization_pending" | "slow_down" => Ok(DevicePoll::Pending),
                other => Err(other.to_string()),
            };
        }

        let student = match provider.kind {
            ProviderKind::Github => {
                let at = body["access_token"].as_str().ok_or("no access_token")?;
                format!("{}:{}", provider.name, self.github_login(at).await?)
            }
            ProviderKind::Oidc => {
                let idt = body["id_token"].as_str().ok_or("no id_token")?;
                let issuer = provider.issuer.as_deref().unwrap();
                let email = self.verify_oidc(issuer, &provider.client_id, idt).await?;
                format!("{}:{}", provider.name, email)
            }
        };
        Ok(DevicePoll::Done(student))
    }
}

/// Verifies an OIDC ID token's signature, issuer, audience, and expiry.
fn verify_id_token(
    id_token: &str,
    jwks: &JwkSet,
    issuer: &str,
    client_id: &str,
) -> Result<OidcClaims, String> {
    let header = decode_header(id_token).map_err(|e| e.to_string())?;
    let kid = header.kid.ok_or("token missing kid")?;
    let jwk = jwks.find(&kid).ok_or("no matching JWK for token kid")?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| e.to_string())?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    decode::<OidcClaims>(id_token, &key, &validation)
        .map(|d| d.claims)
        .map_err(|e| e.to_string())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_secret() -> Identity {
        Identity {
            providers: vec![],
            secret: Some("test-secret".to_string()),
            http: reqwest::Client::new(),
        }
    }

    #[test]
    fn issued_token_round_trips() {
        let id = with_secret();
        let token = id.issue("github:alice").expect("issue");
        assert_eq!(id.verify(&token).as_deref(), Some("github:alice"));
    }

    #[test]
    fn rejects_tampered_or_foreign_tokens() {
        let id = with_secret();
        assert!(id.verify("not-a-jwt").is_none());
        // A token signed with a different secret must not verify.
        let other = Identity {
            providers: vec![],
            secret: Some("other-secret".to_string()),
            http: reqwest::Client::new(),
        };
        let foreign = other.issue("github:mallory").unwrap();
        assert!(id.verify(&foreign).is_none());
    }

    #[test]
    fn disabled_identity_issues_nothing() {
        let id = Identity::disabled();
        assert!(!id.enforced());
        assert!(id.issue("x").is_none());
    }
}
