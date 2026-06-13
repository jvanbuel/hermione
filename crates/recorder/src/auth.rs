//! Obtains a Hermione identity token for the recorder.
//!
//! In GitHub Codespaces the platform `GITHUB_TOKEN` is exchanged silently;
//! otherwise the OAuth device flow is used (print a URL + code, poll until the
//! student approves in a browser). The token is cached so each session doesn't
//! re-authenticate.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub struct Identity {
    pub token: String,
    pub student: String,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    auth_url: String,
    provider: String,
    token: String,
    student: String,
    expiry: i64,
}

#[derive(Deserialize)]
struct ExchangeResp {
    #[serde(rename = "identityToken")]
    identity_token: String,
    student: String,
    #[serde(rename = "expiresIn")]
    expires_in: i64,
}

#[derive(Deserialize)]
struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// Returns a Hermione identity token, from cache or by authenticating.
pub async fn obtain(auth_url: &str, provider: &str) -> anyhow::Result<Identity> {
    let auth_url = auth_url.trim_end_matches('/').to_string();

    if let Some(c) = load_cache() {
        if c.auth_url == auth_url && c.provider == provider && c.expiry > now() + 60 {
            return Ok(Identity {
                token: c.token,
                student: c.student,
            });
        }
    }

    let client = reqwest::Client::new();

    // Codespaces fast-path: exchange the platform GitHub token silently.
    if provider == "github" {
        if let Ok(gh) = std::env::var("GITHUB_TOKEN") {
            if !gh.is_empty() {
                if let Ok(id) = exchange(&client, &auth_url, provider, &gh).await {
                    return Ok(cache_and_return(&auth_url, provider, id));
                }
            }
        }
    }

    // Device flow.
    let start: DeviceStart = client
        .post(format!("{auth_url}/api/auth/device/start"))
        .json(&serde_json::json!({ "provider": provider }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let url = start
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&start.verification_uri);
    eprintln!(
        "\nHermione: sign in to record.\n  → open {url}\n  → enter code: {}\n",
        start.user_code
    );

    let interval = start.interval.max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let resp = client
            .post(format!("{auth_url}/api/auth/device/poll"))
            .json(&serde_json::json!({ "provider": provider, "deviceCode": start.device_code }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!("authorization failed or expired");
        }
        let value: serde_json::Value = resp.json().await?;
        if value.get("status").and_then(|s| s.as_str()) == Some("pending") {
            continue;
        }
        let id: ExchangeResp = serde_json::from_value(value)?;
        eprintln!("Hermione: signed in as {}.", id.student);
        return Ok(cache_and_return(&auth_url, provider, id));
    }
}

async fn exchange(
    client: &reqwest::Client,
    auth_url: &str,
    provider: &str,
    token: &str,
) -> anyhow::Result<ExchangeResp> {
    Ok(client
        .post(format!("{auth_url}/api/auth/exchange"))
        .json(&serde_json::json!({ "provider": provider, "token": token }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn cache_and_return(auth_url: &str, provider: &str, id: ExchangeResp) -> Identity {
    let cached = Cached {
        auth_url: auth_url.to_string(),
        provider: provider.to_string(),
        token: id.identity_token.clone(),
        student: id.student.clone(),
        expiry: now() + id.expires_in,
    };
    save_cache(&cached);
    Identity {
        token: id.identity_token,
        student: id.student,
    }
}

fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/hermione/identity.json"))
}

fn load_cache() -> Option<Cached> {
    let data = std::fs::read(cache_path()?).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_cache(cached: &Cached) {
    let Some(path) = cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_vec(cached) else {
        return;
    };

    // This file holds a bearer identity token, so keep it private to the user —
    // important on shared machines / devcontainers. Create it 0600 so the token
    // is never briefly world-readable.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            // .mode() only applies on creation; tighten an existing file too.
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            let _ = f.write_all(&json);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(&path, json);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
