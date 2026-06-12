//! Hermione backend: a gRPC ingest/viewer server plus an HTTP/SSE web viewer,
//! persisting every terminal session to Postgres.

mod grpc;
mod http;
mod state;
mod text;

use anyhow::Context;
use clap::Parser;
use hermione_migration::{Migrator, MigratorTrait};
use hermione_proto::v1::{ingest_server::IngestServer, viewer_server::ViewerServer};
use sea_orm::Database;
use tonic::transport::Server;

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

    let state = AppState {
        db,
        hub: Hub::default(),
    };

    let grpc_addr = config.grpc_addr.parse().context("parsing grpc address")?;
    let http_addr: std::net::SocketAddr =
        config.http_addr.parse().context("parsing http address")?;

    tracing::info!(%grpc_addr, %http_addr, "hermione backend starting");

    // gRPC server (recorders + native viewers).
    let grpc_state = state.clone();
    let grpc = tokio::spawn(async move {
        Server::builder()
            .add_service(IngestServer::new(IngestService {
                state: grpc_state.clone(),
            }))
            .add_service(ViewerServer::new(ViewerService { state: grpc_state }))
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
