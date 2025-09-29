use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, RwLock};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "register")]
    Register { client_type: ClientType },
    #[serde(rename = "file_update")]
    FileUpdate {
        session_id: String,
        active_file: String,
        file_content: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "session_created")]
    SessionCreated { session_id: String },
    #[serde(rename = "file_updated")]
    FileUpdated {
        session_id: String,
        active_file: String,
        file_content: Option<String>,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClientType {
    #[serde(rename = "vscode")]
    VSCode,
    #[serde(rename = "webapp")]
    WebApp,
}

#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    active_file: Option<String>,
    file_content: Option<String>,
}

#[derive(Debug)]
pub struct Client {
    id: String,
    client_type: ClientType,
    session_id: Option<String>,
    sender: broadcast::Sender<ServerMessage>,
}

pub struct Server {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    clients: Arc<RwLock<HashMap<String, Client>>>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn handle_connection(&self, stream: TcpStream, addr: SocketAddr) {
        info!("New connection from: {}", addr);

        let ws_stream = match accept_async(stream).await {
            Ok(ws_stream) => ws_stream,
            Err(e) => {
                error!("WebSocket connection error: {}", e);
                return;
            }
        };

        let client_id = Uuid::new_v4().to_string();
        let (broadcast_tx, _) = broadcast::channel(100);

        let client = Client {
            id: client_id.clone(),
            client_type: ClientType::WebApp, // Default, will be updated on registration
            session_id: None,
            sender: broadcast_tx.clone(),
        };

        {
            let mut clients = self.clients.write().await;
            clients.insert(client_id.clone(), client);
        }

        if let Err(e) = self.handle_websocket(ws_stream, client_id, broadcast_tx).await {
            error!("WebSocket handler error: {}", e);
        }
    }

    async fn handle_websocket(
        &self,
        ws_stream: WebSocketStream<TcpStream>,
        client_id: String,
        broadcast_tx: broadcast::Sender<ServerMessage>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        let mut broadcast_rx = broadcast_tx.subscribe();

        // Spawn task to handle outgoing messages
        let broadcast_tx_clone = broadcast_tx.clone();
        let outgoing_task = tokio::spawn(async move {
            while let Ok(message) = broadcast_rx.recv().await {
                let json = match serde_json::to_string(&message) {
                    Ok(json) => json,
                    Err(e) => {
                        error!("Failed to serialize message: {}", e);
                        continue;
                    }
                };

                if let Err(e) = ws_sender.send(Message::Text(json)).await {
                    error!("Failed to send message: {}", e);
                    break;
                }
            }
        });

        // Handle incoming messages
        while let Some(msg) = ws_receiver.next().await {
            let msg = match msg {
                Ok(msg) => msg,
                Err(e) => {
                    error!("WebSocket message error: {}", e);
                    break;
                }
            };

            if msg.is_text() || msg.is_binary() {
                let text = msg.to_text()?;

                match serde_json::from_str::<ClientMessage>(text) {
                    Ok(client_message) => {
                        if let Err(e) = self.handle_client_message(client_message, &client_id, &broadcast_tx).await {
                            error!("Error handling client message: {}", e);
                            let error_msg = ServerMessage::Error {
                                message: format!("Error processing message: {}", e),
                            };
                            let _ = broadcast_tx.send(error_msg);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse message: {}", e);
                        let error_msg = ServerMessage::Error {
                            message: format!("Invalid message format: {}", e),
                        };
                        let _ = broadcast_tx.send(error_msg);
                    }
                }
            }
        }

        outgoing_task.abort();

        // Clean up client
        {
            let mut clients = self.clients.write().await;
            clients.remove(&client_id);
        }

        Ok(())
    }

    async fn handle_client_message(
        &self,
        message: ClientMessage,
        client_id: &str,
        broadcast_tx: &broadcast::Sender<ServerMessage>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match message {
            ClientMessage::Register { client_type } => {
                info!("Client {} registered as {:?}", client_id, client_type);

                let session_id = if client_type == ClientType::VSCode {
                    // VSCode creates a new session
                    let session_id = Uuid::new_v4().to_string();
                    let session = Session {
                        id: session_id.clone(),
                        active_file: None,
                        file_content: None,
                    };

                    {
                        let mut sessions = self.sessions.write().await;
                        sessions.insert(session_id.clone(), session);
                    }

                    Some(session_id.clone())
                } else {
                    None
                };

                // Update client
                {
                    let mut clients = self.clients.write().await;
                    if let Some(client) = clients.get_mut(client_id) {
                        client.client_type = client_type;
                        client.session_id = session_id.clone();
                    }
                }

                if let Some(session_id) = session_id {
                    let response = ServerMessage::SessionCreated { session_id };
                    broadcast_tx.send(response)?;
                }
            }

            ClientMessage::FileUpdate { session_id, active_file, file_content } => {
                info!("File update for session {}: {}", session_id, active_file);

                // Update session
                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(session) = sessions.get_mut(&session_id) {
                        session.active_file = Some(active_file.clone());
                        session.file_content = file_content.clone();
                    }
                }

                // Broadcast to all webapp clients
                let clients = self.clients.read().await;
                for client in clients.values() {
                    if client.client_type == ClientType::WebApp {
                        let response = ServerMessage::FileUpdated {
                            session_id: session_id.clone(),
                            active_file: active_file.clone(),
                            file_content: file_content.clone(),
                        };
                        let _ = client.sender.send(response);
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr = "127.0.0.1:8080";
        let listener = TcpListener::bind(addr).await?;
        info!("WebSocket server listening on: {}", addr);

        while let Ok((stream, addr)) = listener.accept().await {
            let server = self.clone();
            tokio::spawn(async move {
                server.handle_connection(stream, addr).await;
            });
        }

        Ok(())
    }
}

impl Clone for Server {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
            clients: Arc::clone(&self.clients),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let server = Server::new();
    server.run().await?;

    Ok(())
}