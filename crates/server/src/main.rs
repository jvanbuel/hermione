//! Hermione backend: a gRPC ingest/viewer server plus an HTTP/SSE web viewer,
//! persisting every terminal session to Postgres.

mod assistant;
mod auth;
mod exercises;
mod files;
mod github_app;
mod grpc;
mod http;
mod identity;
mod messages;
mod repo;
mod state;
mod tenancy;
mod text;

#[cfg(test)]
mod it_tenancy;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hermione_migration::{Migrator, MigratorTrait};
use hermione_proto::v1::{ingest_server::IngestServer, viewer_server::ViewerServer};
use sea_orm::Database;
use tonic::{transport::Server, Request, Status};

use crate::auth::Auth;
use crate::grpc::{IngestService, ViewerService};
use crate::state::{AppState, Hub};

#[derive(Parser, Debug)]
#[command(name = "hermione-server", version, about)]
struct Config {
    /// Postgres connection string.
    #[arg(
        long,
        env = "HERMIONE_DATABASE_URL",
        default_value = "postgres://hermione:hermione@localhost:5432/hermione"
    )]
    database_url: String,

    /// Address for the gRPC ingest/viewer server.
    #[arg(long, env = "HERMIONE_GRPC_ADDR", default_value = "0.0.0.0:50051")]
    grpc_addr: String,

    /// Address for the HTTP/SSE web viewer.
    #[arg(long, env = "HERMIONE_HTTP_ADDR", default_value = "0.0.0.0:8080")]
    http_addr: String,

    /// Super-admin secret for the course/admin provisioning API. If unset, the
    /// provisioning API is disabled.
    #[arg(long, env = "HERMIONE_ADMIN_TOKEN")]
    admin_token: Option<String>,

    /// Username for the admin account seeded on first start.
    #[arg(
        long,
        env = "HERMIONE_BOOTSTRAP_ADMIN_USERNAME",
        default_value = "admin"
    )]
    bootstrap_admin_username: String,

    /// Password for the seeded admin account. When set and no admin accounts
    /// exist yet, that admin is created at startup and granted access to every
    /// existing course — which also takes the dashboard out of open dev mode.
    /// Ignored once any admin exists, so it is safe to leave configured.
    #[arg(long, env = "HERMIONE_BOOTSTRAP_ADMIN_PASSWORD")]
    bootstrap_admin_password: Option<String>,

    /// Anthropic API key for the AI teaching assistant (Managed Agents). If
    /// unset, the assistant is disabled and courses are unaffected.
    #[arg(long, env = "HERMIONE_ANTHROPIC_API_KEY")]
    anthropic_api_key: Option<String>,

    /// Default model for course assistants.
    #[arg(
        long,
        env = "HERMIONE_ASSISTANT_MODEL",
        default_value = "claude-opus-4-8"
    )]
    assistant_model: String,

    /// Reuse a specific Managed Agents environment id instead of creating the
    /// shared `hermione-assistant` one lazily.
    #[arg(long, env = "HERMIONE_ASSISTANT_ENVIRONMENT_ID")]
    assistant_environment_id: Option<String>,

    /// GitHub token used to read a linked repo's folders when seeding a course's
    /// exercises from the dashboard. Optional — needed only for private repos and
    /// to ease rate limits; public repos work without it. The token is only sent
    /// to owners in `--github-allowed-owners`.
    #[arg(long, env = "HERMIONE_GITHUB_TOKEN")]
    github_token: Option<String>,

    /// Comma-separated GitHub owners/orgs whose repos may be read with
    /// `--github-token`. The token is never sent to any other owner, so a teacher
    /// can't point a course at an arbitrary private repo and disclose its folder
    /// names. Empty ⇒ token-backed seeding is off (public repos still seed).
    #[arg(long, env = "HERMIONE_GITHUB_ALLOWED_OWNERS", value_delimiter = ',')]
    github_allowed_owners: Vec<String>,

    /// GitHub App id used to mint repository-scoped installation tokens for
    /// exercise seeding. Paired with `--github-app-private-key-path`, this is the
    /// preferred, most secure option: each seeding request gets a token scoped to
    /// only the linked repo, so a teacher can never disclose an unrelated repo's
    /// folder names. Takes precedence over `--github-token` when the app is
    /// installed on the repo.
    #[arg(long, env = "HERMIONE_GITHUB_APP_ID")]
    github_app_id: Option<String>,

    /// Path to the GitHub App's PEM private key file (pairs with
    /// `--github-app-id`). Both must be set for the app to be used.
    #[arg(long, env = "HERMIONE_GITHUB_APP_PRIVATE_KEY_PATH")]
    github_app_private_key_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermione_server=info,tower_http=info".into()),
        )
        .init();

    let config = Config::parse();

    let db = Database::connect(&config.database_url)
        .await
        .with_context(|| format!("connecting to {}", config.database_url))?;
    Migrator::up(&db, None)
        .await
        .context("running database migrations")?;

    let mut has_admins = tenancy::count_admins(&db).await > 0;

    // Seeding the first admin here closes the window in which a fresh
    // deployment serves an unauthenticated dashboard. It only ever runs on an
    // empty admin table, so restarts and password changes made later through
    // the UI are never clobbered.
    if !has_admins {
        if let Some(password) = config.bootstrap_admin_password.as_deref() {
            let username = &config.bootstrap_admin_username;
            match tenancy::create_admin(&db, username, password).await {
                Ok(admin) => {
                    // Without membership the admin logs in to an empty course
                    // switcher, so grant every course that already exists —
                    // including the seeded `default` one.
                    match tenancy::all_courses(&db, false).await {
                        Ok(courses) => {
                            for course in &courses {
                                if let Err(e) =
                                    tenancy::grant_membership(&db, admin.id, course.id).await
                                {
                                    tracing::warn!(
                                        course = %course.slug,
                                        "could not grant bootstrap admin access: {e}"
                                    );
                                }
                            }
                            tracing::info!(
                                username = %username,
                                courses = courses.len(),
                                "seeded bootstrap admin account"
                            );
                        }
                        Err(e) => tracing::warn!("could not list courses for bootstrap admin: {e}"),
                    }
                    has_admins = true;
                }
                Err(e) => tracing::error!("could not create bootstrap admin: {e}"),
            }
        }
    }

    if !has_admins {
        tracing::warn!(
            "No admin accounts exist — the dashboard is OPEN (scoped to the default \
             course). Set HERMIONE_BOOTSTRAP_ADMIN_PASSWORD, or create an admin via \
             the provisioning API, to lock it down."
        );
    }
    if config.admin_token.is_none() {
        tracing::warn!(
            "HERMIONE_ADMIN_TOKEN is unset — the provisioning API is disabled, so no \
             admins/courses can be created."
        );
    }

    let identity = identity::Identity::from_env();
    if identity.enforced() {
        tracing::info!("verified student identity is ENFORCED (OIDC/GitHub configured)");
    } else {
        tracing::warn!(
            "verified student identity is OFF — `student` is self-asserted. Set \
             HERMIONE_IDENTITY_SECRET and HERMIONE_OIDC_PROVIDERS to enforce it."
        );
    }

    let assistant = assistant::Assistant::new(
        config.anthropic_api_key.clone(),
        config.assistant_environment_id.clone(),
    );
    if assistant.enabled() {
        tracing::info!(
            model = %config.assistant_model,
            "AI teaching assistant is available (per-course opt-in via the dashboard)"
        );
    } else {
        tracing::info!(
            "AI teaching assistant is OFF — set HERMIONE_ANTHROPIC_API_KEY to enable it."
        );
    }

    // A GitHub App (id + private key) mints repo-scoped seeding tokens; it needs
    // both parts and a readable, valid key, else it stays off (public/PAT paths
    // still work).
    let github_app = match (
        config.github_app_id.as_deref(),
        config.github_app_private_key_path.as_deref(),
    ) {
        (Some(id), Some(path)) => match std::fs::read_to_string(path) {
            Ok(pem) => match github_app::GithubApp::new(id, &pem) {
                Ok(app) => {
                    tracing::info!(
                        "GitHub App configured — exercise seeding uses repo-scoped tokens"
                    );
                    Some(app)
                }
                Err(e) => {
                    tracing::error!("GitHub App disabled: {e}");
                    None
                }
            },
            Err(e) => {
                tracing::error!(
                    "GitHub App disabled — could not read private key {}: {e}",
                    path.display()
                );
                None
            }
        },
        (None, None) => None,
        _ => {
            tracing::warn!(
                "GitHub App ignored — set both HERMIONE_GITHUB_APP_ID and \
                 HERMIONE_GITHUB_APP_PRIVATE_KEY_PATH to enable repo-scoped seeding"
            );
            None
        }
    };

    let state = AppState {
        db,
        hub: Hub::default(),
        msg_hub: crate::state::MsgHub::default(),
        auth: Auth::new(),
        identity,
        admin_token: config.admin_token.clone(),
        open_dev: Arc::new(AtomicBool::new(!has_admins)),
        assistant,
        assistant_default_model: config.assistant_model.clone(),
        github_token: config.github_token.clone(),
        github_allowed_owners: config
            .github_allowed_owners
            .iter()
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect(),
        github_app,
    };

    let grpc_addr = config.grpc_addr.parse().context("parsing grpc address")?;
    let http_addr: std::net::SocketAddr =
        config.http_addr.parse().context("parsing http address")?;

    tracing::info!(%grpc_addr, %http_addr, "hermione backend starting");

    // gRPC server. Ingest authenticates per-course via the enrollment token in
    // the handler; the Viewer API is guarded by the super-admin secret.
    let grpc_state = state.clone();
    let admin_token = config.admin_token.clone();
    // tonic interceptors must return Result<_, Status>; Status is large by design.
    #[allow(clippy::result_large_err)]
    let viewer_guard = move |req: Request<()>| -> Result<Request<()>, Status> {
        match &admin_token {
            None => Ok(req),
            Some(expected) => {
                let ok = req
                    .metadata()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|h| h.strip_prefix("Bearer "))
                    .map(|t| auth::constant_time_eq(t.as_bytes(), expected.as_bytes()))
                    .unwrap_or(false);
                if ok {
                    Ok(req)
                } else {
                    Err(Status::unauthenticated("invalid or missing token"))
                }
            }
        }
    };
    let grpc = tokio::spawn(async move {
        Server::builder()
            .add_service(IngestServer::new(IngestService {
                state: grpc_state.clone(),
            }))
            .add_service(ViewerServer::with_interceptor(
                ViewerService { state: grpc_state },
                viewer_guard,
            ))
            .serve(grpc_addr)
            .await
            .context("gRPC server failed")
    });

    // HTTP server (web viewer + SSE).
    let http_app = http::router(state);
    let http = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .context("binding http listener")?;
        axum::serve(listener, http_app)
            .await
            .context("http server failed")
    });

    tokio::select! {
        r = grpc => r.context("grpc task panicked")??,
        r = http => r.context("http task panicked")??,
    }

    Ok(())
}
