//! Hermione backend: a gRPC ingest/viewer server plus an HTTP/SSE web viewer,
//! persisting every terminal session to Postgres.

mod auth;
mod exercises;
mod files;
mod grpc;
mod http;
mod identity;
mod messages;
mod state;
mod tenancy;
mod text;

#[cfg(test)]
mod it_tenancy;

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

    let has_admins = tenancy::count_admins(&db).await > 0;
    if !has_admins {
        tracing::warn!(
            "No admin accounts exist — the dashboard is OPEN (scoped to the default \
             course). Create an admin via the provisioning API to lock it down."
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

    let state = AppState {
        db,
        hub: Hub::default(),
        msg_hub: crate::state::MsgHub::default(),
        auth: Auth::new(),
        identity,
        admin_token: config.admin_token.clone(),
        open_dev: Arc::new(AtomicBool::new(!has_admins)),
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
