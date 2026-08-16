//! Best-effort discovery of a linked repo's exercises via the GitHub API, so a
//! course created from the dashboard is seeded the same way `hermione course
//! create --link` seeds from a local checkout: prefer the repo's
//! `.hermione.json` exercise list, else its top-level folders.
//!
//! Everything here is best-effort — a private repo without a token, a rate
//! limit, or a network hiccup just yields no exercises, never a failed course
//! creation.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Deserialize;

use crate::http::{normalize_slug, title_from_slug};

/// An exercise discovered in a repo (slug + display title).
pub struct DiscoveredExercise {
    pub slug: String,
    pub title: String,
}

/// Parses `(owner, repo)` from a GitHub URL in https or ssh form. Returns `None`
/// for non-GitHub or unparseable URLs (seeding is then simply skipped).
pub fn parse_github(repo_url: &str) -> Option<(String, String)> {
    let s = repo_url.trim();
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("ssh://git@github.com/"))
        .or_else(|| s.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/');
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|p| !p.is_empty())?;
    let repo = parts.next().filter(|p| !p.is_empty())?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    (!repo.is_empty()).then(|| (owner.to_string(), repo.to_string()))
}

#[derive(Deserialize)]
struct ContentEntry {
    #[serde(rename = "type")]
    kind: String,
    name: String,
}

#[derive(Deserialize)]
struct FileContent {
    content: String,
    encoding: String,
}

/// Discovers a GitHub repo's exercises. `token` (a GitHub PAT) is optional but
/// required for private repos and helps with rate limits.
pub async fn discover_exercises(
    owner: &str,
    repo: &str,
    token: Option<&str>,
) -> Result<Vec<DiscoveredExercise>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    // Prefer .hermione.json — the same file the extension resolves against.
    if let Some(cfg) = fetch_hermione_json(&client, token, owner, repo).await {
        let from_config = exercises_from_config(&cfg);
        if !from_config.is_empty() {
            return Ok(from_config);
        }
    }

    // Fall back to top-level folders.
    let mut out: Vec<DiscoveredExercise> = fetch_root(&client, token, owner, repo)
        .await?
        .into_iter()
        .filter(|e| e.kind == "dir" && !is_ignored_dir(&e.name))
        .filter_map(|e| {
            normalize_slug(&e.name).map(|slug| DiscoveredExercise {
                title: title_from_slug(&slug),
                slug,
            })
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out.dedup_by(|a, b| a.slug == b.slug);
    Ok(out)
}

async fn fetch_root(
    client: &reqwest::Client,
    token: Option<&str>,
    owner: &str,
    repo: &str,
) -> Result<Vec<ContentEntry>, String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/");
    let resp = gh_get(client, token, &url).await?;
    match resp.status() {
        s if s.is_success() => resp.json().await.map_err(|e| e.to_string()),
        reqwest::StatusCode::NOT_FOUND => Err(
            "repository not found or not accessible (private repos need HERMIONE_GITHUB_TOKEN)"
                .into(),
        ),
        s => Err(format!("GitHub API returned {s}")),
    }
}

async fn fetch_hermione_json(
    client: &reqwest::Client,
    token: Option<&str>,
    owner: &str,
    repo: &str,
) -> Option<serde_json::Value> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/.hermione.json");
    let resp = gh_get(client, token, &url).await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let file: FileContent = resp.json().await.ok()?;
    if file.encoding != "base64" {
        return None;
    }
    // GitHub wraps base64 content at 60 columns; strip whitespace before decode.
    let cleaned: String = file
        .content
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = BASE64.decode(cleaned.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn gh_get(
    client: &reqwest::Client,
    token: Option<&str>,
    url: &str,
) -> Result<reqwest::Response, String> {
    let mut req = client
        .get(url)
        .header("User-Agent", "hermione")
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    req.send().await.map_err(|e| e.to_string())
}

/// Extracts exercises from a parsed `.hermione.json`, preserving its order.
fn exercises_from_config(cfg: &serde_json::Value) -> Vec<DiscoveredExercise> {
    let Some(arr) = cfg.get("exercises").and_then(|e| e.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<DiscoveredExercise> = Vec::new();
    for ex in arr {
        let Some(name) = ex.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if let Some(slug) = normalize_slug(name) {
            if out.iter().any(|e| e.slug == slug) {
                continue;
            }
            out.push(DiscoveredExercise {
                title: title_from_slug(&slug),
                slug,
            });
        }
    }
    out
}

/// Folders that are never exercises (tooling, build output, VCS, deps).
fn is_ignored_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules"
                | "target"
                | "dist"
                | "build"
                | "out"
                | "bin"
                | "obj"
                | "vendor"
                | "__pycache__"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_urls() {
        assert_eq!(
            parse_github("https://github.com/acme/algorithms-101.git"),
            Some(("acme".into(), "algorithms-101".into()))
        );
        assert_eq!(
            parse_github("git@github.com:acme/data-structures.git"),
            Some(("acme".into(), "data-structures".into()))
        );
        assert_eq!(
            parse_github("https://github.com/acme/repo/tree/main"),
            Some(("acme".into(), "repo".into()))
        );
        assert_eq!(parse_github("https://gitlab.com/acme/repo"), None);
        assert_eq!(parse_github("not a url"), None);
    }

    #[test]
    fn config_order_preserved_and_deduped() {
        let cfg = serde_json::json!({
            "exercises": [
                { "name": "ex3" }, { "name": "ex1" }, { "name": "ex3" }, { "other": 1 }
            ]
        });
        let ex = exercises_from_config(&cfg);
        let slugs: Vec<&str> = ex.iter().map(|e| e.slug.as_str()).collect();
        assert_eq!(slugs, ["ex3", "ex1"]);
    }
}
