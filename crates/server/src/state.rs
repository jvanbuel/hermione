//! Shared application state and the live fan-out hub.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use hermione_proto::v1::TerminalChunk;
use sea_orm::DatabaseConnection;
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

use crate::auth::Auth;

/// Per-session broadcast channels used to fan out live terminal activity to
/// any number of observers (gRPC watchers and SSE web clients).
#[derive(Clone, Default)]
pub struct Hub {
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<TerminalChunk>>>>,
}

impl Hub {
    /// Returns the broadcast sender for a session, creating it if necessary.
    pub async fn channel(&self, id: Uuid) -> broadcast::Sender<TerminalChunk> {
        let mut map = self.channels.write().await;
        map.entry(id)
            .or_insert_with(|| broadcast::channel(4096).0)
            .clone()
    }

    /// Subscribes to live chunks for a session.
    pub async fn subscribe(&self, id: Uuid) -> broadcast::Receiver<TerminalChunk> {
        self.channel(id).await.subscribe()
    }

    /// Drops the channel once a session has ended and no longer needs fan-out.
    pub async fn remove(&self, id: Uuid) {
        self.channels.write().await.remove(&id);
    }
}

/// A message pushed to course members over WebSocket.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageOut {
    pub id: i64,
    pub body: String,
    pub created_at_unix_ms: i64,
}

/// Per-course broadcast channels for live messaging (teacher → students today;
/// the same fan-out underpins two-way chat later).
#[derive(Clone, Default)]
pub struct MsgHub {
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<MessageOut>>>>,
}

impl MsgHub {
    async fn channel(&self, course_id: Uuid) -> broadcast::Sender<MessageOut> {
        let mut map = self.channels.write().await;
        map.entry(course_id)
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }

    pub async fn subscribe(&self, course_id: Uuid) -> broadcast::Receiver<MessageOut> {
        self.channel(course_id).await.subscribe()
    }

    /// Publishes a message to everyone currently connected for the course.
    pub async fn publish(&self, course_id: Uuid, msg: MessageOut) {
        let _ = self.channel(course_id).await.send(msg);
    }
}

/// State shared across the gRPC and HTTP servers.
#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub hub: Hub,
    pub msg_hub: MsgHub,
    pub auth: Auth,
    /// Verified student identity (OIDC / GitHub → Hermione identity token).
    pub identity: crate::identity::Identity,
    /// Super-admin secret for the provisioning API. `None` disables it.
    pub admin_token: Option<String>,
    /// True while no admin accounts exist: the dashboard is open and scoped to
    /// the default course. Flips to false once the first admin is created.
    pub open_dev: Arc<AtomicBool>,
    /// AI teaching assistant (Anthropic Managed Agents). Inert without a key.
    pub assistant: crate::assistant::Assistant,
    /// Default model for newly configured course assistants.
    pub assistant_default_model: String,
    /// GitHub token for reading a linked repo's folders when seeding exercises.
    /// `None` still works for public repos.
    pub github_token: Option<String>,
}
