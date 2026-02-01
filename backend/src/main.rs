use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

// ============================================================================
// Message Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        client_type: ClientType,
        student_name: Option<String>,
    },
    FileUpdate {
        session_id: String,
        file_path: String,
        file_content: Option<String>,
        cursor_position: Option<CursorPosition>,
    },
    TerminalOutput {
        session_id: String,
        output: String,
        stream: TerminalStream,
    },
    TerminalInput {
        session_id: String,
        input: String,
    },
    Heartbeat {
        session_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    SessionCreated {
        session_id: String,
    },
    FileUpdated {
        session_id: String,
        student_name: Option<String>,
        file_path: String,
        file_content: Option<String>,
        cursor_position: Option<CursorPosition>,
        timestamp: DateTime<Utc>,
    },
    TerminalData {
        session_id: String,
        student_name: Option<String>,
        output: String,
        stream: TerminalStream,
        timestamp: DateTime<Utc>,
    },
    TerminalInputReceived {
        session_id: String,
        input: String,
        timestamp: DateTime<Utc>,
    },
    SessionList {
        sessions: Vec<SessionInfo>,
    },
    SessionDisconnected {
        session_id: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    Vscode,
    Shell,
    Webapp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStream {
    Stdout,
    Stderr,
    Stdin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPosition {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub student_name: Option<String>,
    pub client_type: ClientType,
    pub current_file: Option<String>,
    pub connected_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

// ============================================================================
// State Management
// ============================================================================

#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub student_name: Option<String>,
    pub client_type: ClientType,
    pub current_file: Option<String>,
    pub file_content: Option<String>,
    pub cursor_position: Option<CursorPosition>,
    pub connected_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Client {
    pub id: String,
    pub client_type: ClientType,
    pub session_id: Option<String>,
    pub sender: broadcast::Sender<ServerMessage>,
}

pub struct AppState {
    pub sessions: DashMap<String, Session>,
    pub clients: DashMap<String, Client>,
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
}

impl AppState {
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(1000);
        Self {
            sessions: DashMap::new(),
            clients: DashMap::new(),
            broadcast_tx,
        }
    }

    pub fn get_session_list(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|entry| {
                let session = entry.value();
                SessionInfo {
                    session_id: session.id.clone(),
                    student_name: session.student_name.clone(),
                    client_type: session.client_type.clone(),
                    current_file: session.current_file.clone(),
                    connected_at: session.connected_at,
                    last_activity: session.last_activity,
                }
            })
            .collect()
    }

    pub fn broadcast_to_webapps(&self, message: ServerMessage) {
        for client in self.clients.iter() {
            if client.client_type == ClientType::Webapp {
                let _ = client.sender.send(message.clone());
            }
        }
    }
}

// ============================================================================
// WebSocket Handler
// ============================================================================

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let client_id = Uuid::new_v4().to_string();
    let (broadcast_tx, _) = broadcast::channel(100);

    let client = Client {
        id: client_id.clone(),
        client_type: ClientType::Webapp,
        session_id: None,
        sender: broadcast_tx.clone(),
    };

    state.clients.insert(client_id.clone(), client);
    info!("Client {} connected", client_id);

    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = broadcast_tx.subscribe();

    // Task to send messages to client
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if sender.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Handle incoming messages
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(msg) => {
                        handle_client_message(msg, &client_id, &state, &broadcast_tx).await;
                    }
                    Err(e) => {
                        warn!("Failed to parse message from {}: {}", client_id, e);
                        let _ = broadcast_tx.send(ServerMessage::Error {
                            message: format!("Invalid message format: {}", e),
                        });
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                error!("WebSocket error for {}: {}", client_id, e);
                break;
            }
            _ => {}
        }
    }

    // Cleanup
    send_task.abort();

    // Get session_id before removing client
    let session_id = state
        .clients
        .get(&client_id)
        .and_then(|c| c.session_id.clone());

    state.clients.remove(&client_id);

    if let Some(session_id) = session_id {
        state.sessions.remove(&session_id);
        state.broadcast_to_webapps(ServerMessage::SessionDisconnected {
            session_id: session_id.clone(),
        });
        info!("Session {} disconnected", session_id);
    }

    info!("Client {} disconnected", client_id);
}

async fn handle_client_message(
    message: ClientMessage,
    client_id: &str,
    state: &Arc<AppState>,
    client_tx: &broadcast::Sender<ServerMessage>,
) {
    match message {
        ClientMessage::Register {
            client_type,
            student_name,
        } => {
            info!(
                "Client {} registering as {:?} (student: {:?})",
                client_id, client_type, student_name
            );

            let session_id = if client_type == ClientType::Vscode || client_type == ClientType::Shell
            {
                let session_id = Uuid::new_v4().to_string();
                let session = Session {
                    id: session_id.clone(),
                    student_name: student_name.clone(),
                    client_type: client_type.clone(),
                    current_file: None,
                    file_content: None,
                    cursor_position: None,
                    connected_at: Utc::now(),
                    last_activity: Utc::now(),
                };

                state.sessions.insert(session_id.clone(), session);
                Some(session_id)
            } else {
                // Webapp clients receive the current session list
                let sessions = state.get_session_list();
                let _ = client_tx.send(ServerMessage::SessionList { sessions });
                None
            };

            // Update client info
            if let Some(mut client) = state.clients.get_mut(client_id) {
                client.client_type = client_type;
                client.session_id = session_id.clone();
            }

            if let Some(session_id) = session_id {
                let _ = client_tx.send(ServerMessage::SessionCreated {
                    session_id: session_id.clone(),
                });

                // Notify webapps of new session
                let sessions = state.get_session_list();
                state.broadcast_to_webapps(ServerMessage::SessionList { sessions });
            }
        }

        ClientMessage::FileUpdate {
            session_id,
            file_path,
            file_content,
            cursor_position,
        } => {
            let student_name = if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.current_file = Some(file_path.clone());
                session.file_content = file_content.clone();
                session.cursor_position = cursor_position.clone();
                session.last_activity = Utc::now();
                session.student_name.clone()
            } else {
                None
            };

            info!(
                "File update for session {}: {}",
                session_id, file_path
            );

            state.broadcast_to_webapps(ServerMessage::FileUpdated {
                session_id,
                student_name,
                file_path,
                file_content,
                cursor_position,
                timestamp: Utc::now(),
            });
        }

        ClientMessage::TerminalOutput {
            session_id,
            output,
            stream,
        } => {
            let student_name = if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.last_activity = Utc::now();
                session.student_name.clone()
            } else {
                None
            };

            state.broadcast_to_webapps(ServerMessage::TerminalData {
                session_id,
                student_name,
                output,
                stream,
                timestamp: Utc::now(),
            });
        }

        ClientMessage::TerminalInput {
            session_id,
            input,
        } => {
            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.last_activity = Utc::now();
            }

            state.broadcast_to_webapps(ServerMessage::TerminalInputReceived {
                session_id,
                input,
                timestamp: Utc::now(),
            });
        }

        ClientMessage::Heartbeat { session_id } => {
            if let Some(mut session) = state.sessions.get_mut(&session_id) {
                session.last_activity = Utc::now();
            }
        }
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermione_backend=info,tower_http=info".into()),
        )
        .init();

    let state = Arc::new(AppState::new());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "OK" }))
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:8080";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    info!("Hermione backend listening on {}", addr);

    axum::serve(listener, app).await.unwrap();
}
