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

/// A control frame pushed to one student's editor over the message socket.
///
/// Ephemeral by design: control frames are never persisted and are dropped
/// entirely when that student has no editor connected. Both the extension and
/// the dashboard key on `body` to decide a frame is a broadcast, so a frame
/// carrying only `kind` is ignored by clients that predate this.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOut {
    pub kind: String,
}

type CtrlChannels = HashMap<(Uuid, String), broadcast::Sender<ControlOut>>;

/// Per-student control channels, used to ask one student's editor for a
/// snapshot of the file they have open.
///
/// Keyed by student as well as course so a request for Alice never reaches
/// Bob's editor — who is being watched is not something the rest of the class
/// should learn. The key comes from the socket's self-asserted `student`
/// parameter, which is only ever used for routing: the snapshot that comes
/// back is attributed by the ingest gate's verified identity, not by this.
#[derive(Clone, Default)]
pub struct CtrlHub {
    channels: Arc<RwLock<CtrlChannels>>,
}

impl CtrlHub {
    /// Subscribes one connected editor to its student's control frames.
    pub async fn subscribe(
        &self,
        course_id: Uuid,
        student: &str,
    ) -> broadcast::Receiver<ControlOut> {
        let mut map = self.channels.write().await;
        map.entry((course_id, student.to_string()))
            .or_insert_with(|| broadcast::channel(16).0)
            .clone()
            .subscribe()
    }

    /// Sends a control frame to a student's editors. Returns false when nobody
    /// is listening, so a caller can tell "no editor connected" from "asked".
    pub async fn publish(&self, course_id: Uuid, student: &str, frame: ControlOut) -> bool {
        let key = (course_id, student.to_string());
        let sender = { self.channels.read().await.get(&key).cloned() };
        let Some(sender) = sender else {
            return false;
        };
        if sender.send(frame).is_ok() {
            return true;
        }
        // The last editor for this student disconnected; drop the channel
        // rather than let the map grow for the life of the process.
        self.channels.write().await.remove(&key);
        false
    }

    /// Drops a student's channel once their last editor has disconnected.
    pub async fn release(&self, course_id: Uuid, student: &str) {
        let mut map = self.channels.write().await;
        let key = (course_id, student.to_string());
        if map.get(&key).is_some_and(|s| s.receiver_count() == 0) {
            map.remove(&key);
        }
    }
}

/// State shared across the gRPC and HTTP servers.
#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub hub: Hub,
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
