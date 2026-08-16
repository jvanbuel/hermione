//! `hermione course …` — manage courses over the HTTP API.
//!
//! Kept separate from the recorder's argument parsing so the transparent
//! `hermione -- <cmd>` recording path is untouched: `main` dispatches here only
//! when the first argument is literally `course`.
//!
//! A teacher can create a course, or link an existing git repository as one
//! (`--repo`, or `--link` to read the current repo's `origin` remote). Auth is
//! either the provisioning secret (`--admin-token`, hits `/api/admin/courses`)
//! or a teacher sign-in (`--user` + password, hits `/api/courses`).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hermione course", about = "Manage Hermione courses")]
struct CourseCli {
    #[command(subcommand)]
    cmd: CourseCmd,
}

#[derive(Subcommand, Debug)]
enum CourseCmd {
    /// Create a new course, or link an existing git repo as one.
    Create(CreateArgs),
    /// Scaffold a `.hermione.json` from a repo's top-level folders, so the
    /// exercise mapping lives in the repo (the extension reads it, and
    /// `course create --link` seeds from it).
    Init(InitArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Directory to scan and write `.hermione.json` into (default: current dir).
    #[arg(long)]
    dir: Option<String>,

    /// Overwrite an existing `.hermione.json`.
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug)]
struct CreateArgs {
    /// Course slug (its URL id). Derived from --repo/--link or --name if omitted.
    slug: Option<String>,

    /// Human-friendly course name (derived from the slug/repo if omitted).
    #[arg(long)]
    name: Option<String>,

    /// Link a git repository to the course (its URL).
    #[arg(long)]
    repo: Option<String>,

    /// Derive the repo, slug, and name from the current git repo's `origin`.
    #[arg(long)]
    link: bool,

    /// Don't seed the course's exercises from the repo's folders (see --link).
    #[arg(long)]
    no_exercises: bool,

    /// HTTP base URL of the Hermione server.
    #[arg(
        long,
        env = "HERMIONE_SERVER_URL",
        default_value = "http://127.0.0.1:8080"
    )]
    server: String,

    /// Provisioning secret — uses the admin API. Takes precedence over --user.
    #[arg(long, env = "HERMIONE_ADMIN_TOKEN")]
    admin_token: Option<String>,

    /// Teacher username to sign in with (uses the teacher API).
    #[arg(long, env = "HERMIONE_USER")]
    user: Option<String>,

    /// Teacher password. Prompted for (no echo) if omitted.
    #[arg(long, env = "HERMIONE_PASSWORD")]
    password: Option<String>,
}

/// Entry point for `hermione course …`. `argv[0]` is the program name (the
/// `course` token has already been stripped by the caller).
pub async fn run(argv: Vec<String>) -> Result<()> {
    let cli = CourseCli::parse_from(argv);
    match cli.cmd {
        CourseCmd::Create(args) => create(args).await,
        CourseCmd::Init(args) => init(args),
    }
}

async fn create(args: CreateArgs) -> Result<()> {
    // Resolve the repo: an explicit --repo wins; otherwise --link reads origin.
    let repo = match args.repo.clone() {
        Some(r) if !r.trim().is_empty() => Some(r.trim().to_string()),
        _ if args.link => Some(git_origin()?),
        _ => None,
    };

    // Resolve the slug: explicit positional, else from the repo, else the name.
    let slug = args
        .slug
        .as_deref()
        .and_then(normalize_slug)
        .or_else(|| {
            repo.as_deref()
                .and_then(repo_short_name)
                .as_deref()
                .and_then(normalize_slug)
        })
        .or_else(|| args.name.as_deref().and_then(normalize_slug))
        .ok_or_else(|| anyhow!("provide a slug, --name, --repo, or --link"))?;

    let name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| title_from_slug(&slug));

    // When linking, we're sitting in the course repo — so its folders (or its
    // `.hermione.json`) are the exercises. Discover them up front to report on.
    let exercises = if args.link && !args.no_exercises {
        discover_exercises().unwrap_or_default()
    } else {
        Vec::new()
    };

    let server = args.server.trim_end_matches('/').to_string();
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "slug": slug, "name": name, "repoUrl": repo });

    // Auth determines both the create endpoint and whether we hold a teacher
    // session (needed to also define exercises, which is a teacher-only route).
    let (resp, cookie) = if let Some(token) = args.admin_token.as_deref().filter(|t| !t.is_empty())
    {
        let resp = client
            .post(format!("{server}/api/admin/courses"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;
        (resp, None)
    } else if let Some(user) = args.user.as_deref().filter(|u| !u.is_empty()) {
        let cookie = login(&client, &server, user, args.password.clone()).await?;
        let resp = client
            .post(format!("{server}/api/courses"))
            .header(reqwest::header::COOKIE, &cookie)
            .json(&body)
            .send()
            .await?;
        (resp, Some(cookie))
    } else {
        bail!("authenticate with --admin-token (HERMIONE_ADMIN_TOKEN) or --user (HERMIONE_USER)");
    };

    let created = created_course(resp).await?;
    report_created(&created, repo.as_deref());

    // Seed the course's exercises from the repo. `/api/exercises` is a
    // teacher-only route, so this needs a sign-in — under --admin-token we say so
    // rather than silently dropping the exercises.
    if !exercises.is_empty() {
        match &cookie {
            Some(cookie) => {
                seed_exercises(&client, &server, cookie, &created.slug, &exercises).await?;
                let names: Vec<&str> = exercises.iter().map(|e| e.slug.as_str()).collect();
                println!(
                    "Defined {} exercise(s): {}",
                    exercises.len(),
                    names.join(", ")
                );
            }
            None => {
                eprintln!(
                    "Note: found {} exercise folder(s) but didn't define them — \
                     exercise seeding needs a teacher sign-in (--user).",
                    exercises.len()
                );
            }
        }
    }
    Ok(())
}

/// `hermione course init` — write a `.hermione.json` mapping each top-level
/// folder to an exercise, so the mapping lives in the repo.
fn init(args: InitArgs) -> Result<()> {
    let dir = args
        .dir
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let path = dir.join(".hermione.json");

    let mut folders = scan_exercise_folders(&dir)?;
    folders.sort();
    // A folder whose name contains a glob wildcard (`*`/`?`) can't be written as a
    // literal `match` in .hermione.json's glob grammar, so skip it (with a note)
    // rather than emit a pattern that would also match unrelated folders.
    let (usable, skipped): (Vec<String>, Vec<String>) =
        folders.into_iter().partition(|f| !f.contains(['*', '?']));
    for f in &skipped {
        eprintln!("Skipping {f:?}: folder name contains a glob wildcard (* or ?).");
    }

    let mut rules = exercise_rules_for_folders(&usable);
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    if rules.is_empty() {
        bail!(
            "no exercise folders found in {} — create a folder per exercise, then re-run",
            dir.display()
        );
    }

    // Serialize from the struct (not a json! Value) so keys read name-then-match.
    let text = serde_json::to_string_pretty(&HermioneConfig { exercises: &rules })? + "\n";
    write_config(&path, &text, args.force)?;
    let names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
    println!(
        "Wrote {} with {} exercise(s): {}",
        path.display(),
        names.len(),
        names.join(", ")
    );
    println!("  Commit it, then: hermione course create --link --user <you>");
    Ok(())
}

/// Writes the config without following a symlink out of the target directory.
/// Without `--force`, uses `O_EXCL` (create-new) so an existing path — a dangling
/// `.hermione.json` symlink included — fails cleanly instead of redirecting the
/// write. With `--force`, refuses to overwrite a symlink but replaces a file.
fn write_config(path: &Path, text: &str, force: bool) -> Result<()> {
    use std::io::Write;
    if force {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                bail!("{} is a symlink — refusing to overwrite it", path.display());
            }
        }
        std::fs::write(path, text)?;
    } else {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => f.write_all(text.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!(
                    "{} already exists — pass --force to overwrite",
                    path.display()
                );
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Top-level directories in `dir` that could be exercises (skips hidden and
/// tooling/build/deps dirs).
fn scan_exercise_folders(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_ignored_dir(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

/// One `.hermione.json` exercise rule: an exercise `name` (slug) and the glob it
/// `match`es. Field order is preserved on serialize (name, then match).
#[derive(serde::Serialize, Debug, PartialEq)]
struct ExerciseRule {
    name: String,
    #[serde(rename = "match")]
    pattern: String,
}

#[derive(serde::Serialize)]
struct HermioneConfig<'a> {
    exercises: &'a [ExerciseRule],
}

/// Builds one exercise rule per folder — `name` the folder's slug, `match` a
/// glob over that folder — deduped by slug, in the order given.
fn exercise_rules_for_folders(folders: &[String]) -> Vec<ExerciseRule> {
    let mut seen = std::collections::HashSet::new();
    folders
        .iter()
        .filter_map(|folder| {
            let slug = normalize_slug(folder)?;
            seen.insert(slug.clone()).then(|| ExerciseRule {
                name: slug,
                pattern: format!("{folder}/**"),
            })
        })
        .collect()
}

/// Signs in and returns the `hermione_session=…` cookie for the teacher API.
async fn login(
    client: &reqwest::Client,
    server: &str,
    user: &str,
    password: Option<String>,
) -> Result<String> {
    let password = match password.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => prompt_password(&format!("Password for {user}: "))?,
    };
    let resp = client
        .post(format!("{server}/api/login"))
        .json(&serde_json::json!({ "username": user, "password": password }))
        .send()
        .await?;
    if resp.status() != reqwest::StatusCode::NO_CONTENT {
        bail!("sign-in failed for '{user}' ({})", resp.status());
    }
    resp.headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| c.split(';').next())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("server did not return a session cookie"))
}

/// The course a create request returned.
struct Created {
    slug: String,
    name: String,
    token: String,
}

/// Parses a create response, turning a non-2xx into a readable error.
async fn created_course(resp: reqwest::Response) -> Result<Created> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("server rejected the request ({status}): {}", text.trim());
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    Ok(Created {
        slug: v["slug"].as_str().unwrap_or_default().to_string(),
        name: v["name"].as_str().unwrap_or_default().to_string(),
        token: v["enrollmentToken"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

/// Prints the new course and the enrollment token students need to join.
fn report_created(c: &Created, repo: Option<&str>) {
    println!("Created course '{}' ({}).", c.slug, c.name);
    if let Some(repo) = repo {
        println!("Linked repo: {repo}");
    }
    if !c.token.is_empty() {
        println!("Enrollment token: {}", c.token);
        println!(
            "  Students enroll with: hermione --token {} --backend <grpc-url>",
            c.token
        );
    }
}

/// An exercise discovered in the course repo.
struct DiscoveredExercise {
    slug: String,
    title: String,
}

/// Discovers the repo's exercises from the current directory: a `.hermione.json`
/// (the same file the extension reads) if present, otherwise each top-level
/// folder. Ordering follows the config, or alphabetical for folders.
fn discover_exercises() -> Result<Vec<DiscoveredExercise>> {
    if let Ok(bytes) = std::fs::read(".hermione.json") {
        if let Ok(cfg) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let from_config = exercises_from_config(&cfg);
            if !from_config.is_empty() {
                return Ok(from_config);
            }
        }
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(".")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if is_ignored_dir(&name) {
            continue;
        }
        if let Some(slug) = normalize_slug(&name) {
            out.push(DiscoveredExercise {
                title: title_from_slug(&slug),
                slug,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out.dedup_by(|a, b| a.slug == b.slug);
    Ok(out)
}

/// Extracts exercises from a parsed `.hermione.json`, preserving its order.
fn exercises_from_config(cfg: &serde_json::Value) -> Vec<DiscoveredExercise> {
    let Some(arr) = cfg.get("exercises").and_then(|e| e.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ex in arr {
        let Some(name) = ex.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if let Some(slug) = normalize_slug(name) {
            if out.iter().any(|e: &DiscoveredExercise| e.slug == slug) {
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

/// Defines (upserts) the discovered exercises on the course. Requires a teacher
/// session cookie — `/api/exercises` is a teacher-only route.
async fn seed_exercises(
    client: &reqwest::Client,
    server: &str,
    cookie: &str,
    course_slug: &str,
    exercises: &[DiscoveredExercise],
) -> Result<()> {
    let items: Vec<serde_json::Value> = exercises
        .iter()
        .enumerate()
        .map(|(i, e)| serde_json::json!({ "slug": e.slug, "title": e.title, "position": i }))
        .collect();
    let body = serde_json::json!({ "course": course_slug, "exercises": items });
    let resp = client
        .post(format!("{server}/api/exercises"))
        .header(reqwest::header::COOKIE, cookie)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!(
            "course created, but defining its exercises failed ({status}): {}",
            text.trim()
        );
    }
    Ok(())
}

/// The current git repository's `origin` remote URL.
fn git_origin() -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| anyhow!("running git: {e}"))?;
    if !out.status.success() {
        bail!(
            "could not read git 'origin' remote — run inside a repo with an origin, or pass --repo"
        );
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        bail!("git 'origin' remote is empty");
    }
    Ok(url)
}

/// Reads a password from the terminal without echoing it.
fn prompt_password(prompt: &str) -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyModifiers};
    use std::io::Write;

    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    crossterm::terminal::enable_raw_mode()?;
    let mut pw = String::new();
    let result = loop {
        match crossterm::event::read() {
            Ok(Event::Key(k)) => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow!("cancelled"))
                }
                KeyCode::Backspace => {
                    pw.pop();
                }
                KeyCode::Char(c) => pw.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow!("reading password: {e}")),
        }
    };
    crossterm::terminal::disable_raw_mode().ok();
    eprintln!();
    result.map(|()| pw)
}

/// Normalizes a slug to a URL-safe form (`[a-z0-9-]`), collapsing other runs to
/// single dashes. Returns `None` if nothing usable remains. Mirrors the server.
fn normalize_slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// The repository's short name — the last path segment of a git URL minus `.git`.
/// Handles `https://host/owner/name.git` and `git@host:owner/name.git`.
fn repo_short_name(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let tail = trimmed.rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Turns a slug into a human title ("intro-python" → "Intro Python").
fn title_from_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_normalization() {
        assert_eq!(
            normalize_slug("Intro Python").as_deref(),
            Some("intro-python")
        );
        assert_eq!(normalize_slug("CS 101!!").as_deref(), Some("cs-101"));
        assert_eq!(normalize_slug("--a__b--").as_deref(), Some("a-b"));
        assert_eq!(normalize_slug("   ").as_deref(), None);
        assert_eq!(normalize_slug("!!!").as_deref(), None);
    }

    #[test]
    fn exercise_rules_map_folders_to_globs() {
        let folders = vec![
            "ex1-arrays".to_string(),
            "Ex1 Arrays".to_string(), // duplicate slug — dropped
            "week 02".to_string(),
        ];
        let rules = exercise_rules_for_folders(&folders);
        let names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
        // First "ex1-arrays" wins; the dup is dropped. Order preserved.
        assert_eq!(names, ["ex1-arrays", "week-02"]);
        // `match` uses the original folder name (real path).
        assert_eq!(rules[0].pattern, "ex1-arrays/**");
        assert_eq!(rules[1].pattern, "week 02/**");
        // Serializes with name before match, and only these keys.
        let json = serde_json::to_string(&rules[0]).unwrap();
        assert_eq!(json, r#"{"name":"ex1-arrays","match":"ex1-arrays/**"}"#);
    }

    /// A unique scratch directory for a filesystem test.
    fn scratch(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("hermione-init-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_writes_sorted_and_skips_wildcard_folders() {
        let dir = scratch("sorted");
        for f in ["Zebra", "apple", "ex*"] {
            std::fs::create_dir_all(dir.join(f)).unwrap();
        }
        init(InitArgs {
            dir: Some(dir.to_string_lossy().into_owned()),
            force: false,
        })
        .unwrap();

        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(".hermione.json")).unwrap()).unwrap();
        let names: Vec<&str> = doc["exercises"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        // Sorted by slug; the `ex*` folder (glob wildcard) is skipped.
        assert_eq!(names, ["apple", "zebra"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn init_refuses_to_write_through_a_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let dir = scratch("symlink");
        std::fs::create_dir_all(dir.join("ex1")).unwrap();
        let target = dir.join("outside-target.json");
        symlink(&target, dir.join(".hermione.json")).unwrap();

        let res = init(InitArgs {
            dir: Some(dir.to_string_lossy().into_owned()),
            force: false,
        });
        assert!(res.is_err(), "must refuse to follow a dangling symlink");
        assert!(!target.exists(), "must not create the symlink target");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repo_names() {
        assert_eq!(
            repo_short_name("https://github.com/org/intro-python.git").as_deref(),
            Some("intro-python")
        );
        assert_eq!(
            repo_short_name("git@github.com:org/CS101.git").as_deref(),
            Some("CS101")
        );
        assert_eq!(
            repo_short_name("https://gitlab.com/g/sub/course/").as_deref(),
            Some("course")
        );
    }

    #[test]
    fn titles() {
        assert_eq!(title_from_slug("intro-python"), "Intro Python");
        assert_eq!(title_from_slug("cs-101"), "Cs 101");
    }

    #[test]
    fn exercises_from_hermione_json() {
        let cfg = serde_json::json!({
            "exercises": [
                { "name": "ex1", "match": "ex1/**" },
                { "name": "Strings", "match": ["strings/**"] },
                { "name": "ex1", "match": "dup/**" },
                { "match": "nameless/**" }
            ]
        });
        let ex = exercises_from_config(&cfg);
        // Config order preserved; duplicate slug and nameless entry dropped.
        let slugs: Vec<&str> = ex.iter().map(|e| e.slug.as_str()).collect();
        assert_eq!(slugs, ["ex1", "strings"]);
        assert_eq!(ex[1].title, "Strings");
    }

    #[test]
    fn empty_config_yields_nothing() {
        assert!(exercises_from_config(&serde_json::json!({})).is_empty());
        assert!(exercises_from_config(&serde_json::json!({ "exercises": [] })).is_empty());
    }

    #[test]
    fn ignores_tooling_dirs() {
        for d in [
            ".git",
            ".github",
            "node_modules",
            "target",
            "dist",
            "__pycache__",
        ] {
            assert!(is_ignored_dir(d), "{d} should be ignored");
        }
        for d in ["ex1", "pointers", "week-01"] {
            assert!(!is_ignored_dir(d), "{d} should count");
        }
    }
}
