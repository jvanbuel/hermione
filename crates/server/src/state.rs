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

/// A set of broadcast channels keyed by whatever the fan-out is per.
///
/// Three things in the server need exactly this: live terminal chunks per
/// session, course broadcasts, and control frames per student. A channel is
/// created on first subscribe and dropped once its last listener goes, so the
/// map tracks who is actually listening rather than everything ever seen.
pub struct Hub<K, T, const CAP: usize> {
    channels: Arc<RwLock<HashMap<K, broadcast::Sender<T>>>>,
}

// Derived impls would demand `K: Clone`/`T: Default` and similar bounds that
// an `Arc` field doesn't actually need.
impl<K, T, const CAP: usize> Clone for Hub<K, T, CAP> {
    fn clone(&self) -> Self {
        Self {
            channels: Arc::clone(&self.channels),
        }
    }
}

impl<K, T, const CAP: usize> Default for Hub<K, T, CAP> {
    fn default() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl<K: Eq + std::hash::Hash + Clone, T: Clone, const CAP: usize> Hub<K, T, CAP> {
    /// The sender for a key, creating the channel if nobody has used it yet.
    pub async fn channel(&self, key: &K) -> broadcast::Sender<T> {
        self.channels
            .write()
            .await
            .entry(key.clone())
            .or_insert_with(|| broadcast::channel(CAP).0)
            .clone()
    }

    /// Subscribes a listener.
    pub async fn subscribe(&self, key: &K) -> broadcast::Receiver<T> {
        self.channel(key).await.subscribe()
    }

    /// Sends to everyone currently listening on `key`, and forgets the channel
    /// when it turns out nobody is — otherwise the map would grow for the life
    /// of the process.
    pub async fn publish(&self, key: &K, msg: T) {
        let sender = { self.channels.read().await.get(key).cloned() };
        if let Some(sender) = sender {
            if sender.send(msg).is_err() {
                self.channels.write().await.remove(key);
            }
        }
    }

    /// How many listeners `key` has right now. Zero also covers "never seen".
    pub async fn listeners(&self, key: &K) -> usize {
        self.channels
            .read()
            .await
            .get(key)
            .map_or(0, |s| s.receiver_count())
    }

    /// Drops a channel outright, for a key that can never be used again.
    pub async fn remove(&self, key: &K) {
        self.channels.write().await.remove(key);
    }
}

/// Live terminal activity, fanned out to gRPC watchers and SSE web clients.
pub type SessionHub = Hub<Uuid, TerminalChunk, 4096>;

/// Course messaging (teacher → students today; the same fan-out underpins
/// two-way chat later).
pub type MsgHub = Hub<Uuid, MessageOut, 256>;

/// Control frames addressed to one student's editor.
///
/// Keyed by student as well as course so a request for Alice never reaches
/// Bob's editor — who is being watched is not something the rest of the class
/// should learn. The key comes from the socket's self-asserted `student`
/// parameter, which is only ever used for routing: the snapshot that comes
/// back is attributed by the ingest gate's verified identity, not by this.
pub type CtrlHub = Hub<(Uuid, String), ControlOut, 16>;

/// A message pushed to course members over WebSocket.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageOut {
    pub id: i64,
    pub body: String,
    pub created_at_unix_ms: i64,
}

/// A control frame pushed to one student's editor over the message socket.
///
/// Ephemeral by design: control frames are never persisted and are dropped
/// entirely when that student has no editor connected. Both the extension and
/// the dashboard key on `body` to decide a frame is a broadcast, so a frame
/// carrying only `kind` is ignored by clients that predate this.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOut {
    pub kind: &'static str,
}

/// State shared across the gRPC and HTTP servers.
#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub hub: SessionHub,
    pub msg_hub: MsgHub,
    /// Teacher → one student's editor control frames (snapshot requests).
    pub ctrl_hub: CtrlHub,
    /// Latest file snapshot per student. In memory and short-lived: what a
    /// student currently has on screen is for live intervention, not a record.
    pub snapshots: crate::snapshots::SnapshotStore,
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
    /// Owners/orgs whose repos may be read with `github_token`. The token is
    /// never sent to any other owner (guards against cross-repo disclosure).
    pub github_allowed_owners: Vec<String>,
    /// GitHub App for minting repository-scoped installation tokens when seeding
    /// exercises. Preferred over `github_token`: it reads only the linked repo,
    /// enforcing the authorization boundary at runtime. `None` disables it.
    pub github_app: Option<crate::github_app::GithubApp>,
}
