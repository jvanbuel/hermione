//! gRPC services: `Ingest` (recorders push) and `Viewer` (observers pull).

use std::pin::Pin;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{Stream, StreamExt};
use hermione_entity::{sessions, terminal_events};
use hermione_proto::v1::{
    ingest_event::Event,
    ingest_server::Ingest,
    viewer_server::Viewer,
    IngestEvent, IngestSummary, ListSessionsRequest, ListSessionsResponse, SessionInfo,
    StreamKind, TerminalChunk, WatchRequest,
};
use sea_orm::{
    ActiveValue::{Set, Unchanged},
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use tokio::sync::broadcast;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::state::AppState;

pub struct IngestService {
    pub state: AppState,
}

pub struct ViewerService {
    pub state: AppState,
}

#[tonic::async_trait]
impl Ingest for IngestService {
    async fn stream_session(
        &self,
        request: Request<Streaming<IngestEvent>>,
    ) -> Result<Response<IngestSummary>, Status> {
        let mut stream = request.into_inner();
        let db = &self.state.db;

        let mut session_id: Option<Uuid> = None;
        let mut bcast: Option<broadcast::Sender<TerminalChunk>> = None;
        let mut seq: i64 = 0;
        let mut ended = false;

        while let Some(item) = stream.next().await {
            match item?.event {
                Some(Event::Start(start)) => {
                    let id = Uuid::new_v4();
                    let model = sessions::ActiveModel {
                        id: Set(id),
                        student: Set(start.student),
                        command: Set(start.command),
                        hostname: Set(non_empty(start.hostname)),
                        cols: Set(start.cols as i32),
                        rows: Set(start.rows as i32),
                        status: Set("active".to_string()),
                        started_at: Set(chrono::Utc::now().into()),
                        ended_at: Set(None),
                        exit_code: Set(None),
                    };
                    sessions::Entity::insert(model)
                        .exec(db)
                        .await
                        .map_err(internal)?;
                    bcast = Some(self.state.hub.channel(id).await);
                    session_id = Some(id);
                    tracing::info!(%id, "session started");
                }

                Some(Event::Chunk(chunk)) => {
                    let Some(id) = session_id else { continue };
                    let kind = stream_label(chunk.stream);
                    let model = terminal_events::ActiveModel {
                        session_id: Set(id),
                        seq: Set(seq),
                        offset_ms: Set(chunk.offset_ms),
                        stream: Set(kind.to_string()),
                        data: Set(BASE64.encode(&chunk.data)),
                        text: Set(Some(crate::text::plain(&chunk.data))),
                        created_at: Set(chrono::Utc::now().into()),
                        ..Default::default()
                    };
                    terminal_events::Entity::insert(model)
                        .exec(db)
                        .await
                        .map_err(internal)?;
                    seq += 1;
                    if let Some(sender) = &bcast {
                        let _ = sender.send(chunk);
                    }
                }

                Some(Event::Resize(resize)) => {
                    if let Some(id) = session_id {
                        let model = sessions::ActiveModel {
                            id: Unchanged(id),
                            cols: Set(resize.cols as i32),
                            rows: Set(resize.rows as i32),
                            ..Default::default()
                        };
                        let _ = sessions::Entity::update(model).exec(db).await;
                    }
                }

                Some(Event::End(end)) => {
                    if let Some(id) = session_id {
                        finalize(db, id, Some(end.exit_code)).await;
                        ended = true;
                    }
                }

                None => {}
            }
        }

        if let Some(id) = session_id {
            if !ended {
                finalize(db, id, None).await;
            }
            self.state.hub.remove(id).await;
            tracing::info!(%id, chunks = seq, "session ingest finished");
        }

        Ok(Response::new(IngestSummary {
            session_id: session_id.map(|i| i.to_string()).unwrap_or_default(),
            chunks_received: seq as u64,
        }))
    }
}

#[tonic::async_trait]
impl Viewer for ViewerService {
    async fn list_sessions(
        &self,
        _request: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        let rows = sessions::Entity::find()
            .order_by_desc(sessions::Column::StartedAt)
            .all(&self.state.db)
            .await
            .map_err(internal)?;

        Ok(Response::new(ListSessionsResponse {
            sessions: rows.into_iter().map(session_info).collect(),
        }))
    }

    type WatchSessionStream =
        Pin<Box<dyn Stream<Item = Result<TerminalChunk, Status>> + Send + 'static>>;

    async fn watch_session(
        &self,
        request: Request<WatchRequest>,
    ) -> Result<Response<Self::WatchSessionStream>, Status> {
        let req = request.into_inner();
        let id = Uuid::parse_str(&req.session_id)
            .map_err(|_| Status::invalid_argument("invalid session_id"))?;

        let rx = self.state.hub.subscribe(id).await;
        let db = self.state.db.clone();
        let include_history = req.include_history;

        let output = async_stream::try_stream! {
            if include_history {
                let rows = terminal_events::Entity::find()
                    .filter(terminal_events::Column::SessionId.eq(id))
                    .order_by_asc(terminal_events::Column::Seq)
                    .all(&db)
                    .await
                    .map_err(internal)?;
                for row in rows {
                    yield decode_chunk(&row);
                }
            }

            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(chunk) => yield chunk,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        };

        Ok(Response::new(Box::pin(output)))
    }
}

// --- helpers ---------------------------------------------------------------

async fn finalize(db: &DatabaseConnection, id: Uuid, exit_code: Option<i32>) {
    let model = sessions::ActiveModel {
        id: Unchanged(id),
        status: Set("ended".to_string()),
        ended_at: Set(Some(chrono::Utc::now().into())),
        exit_code: Set(exit_code),
        ..Default::default()
    };
    if let Err(e) = sessions::Entity::update(model).exec(db).await {
        tracing::warn!(%id, error = %e, "failed to finalize session");
    }
}

fn stream_label(stream: i32) -> &'static str {
    match StreamKind::try_from(stream) {
        Ok(StreamKind::Stdin) => "stdin",
        _ => "stdout",
    }
}

fn decode_chunk(row: &terminal_events::Model) -> TerminalChunk {
    let stream = if row.stream == "stdin" {
        StreamKind::Stdin
    } else {
        StreamKind::Stdout
    };
    TerminalChunk {
        stream: stream as i32,
        data: BASE64.decode(row.data.as_bytes()).unwrap_or_default(),
        offset_ms: row.offset_ms,
    }
}

fn session_info(row: sessions::Model) -> SessionInfo {
    SessionInfo {
        session_id: row.id.to_string(),
        student: row.student,
        command: row.command,
        status: row.status,
        started_at_unix_ms: row.started_at.timestamp_millis(),
        ended_at_unix_ms: row.ended_at.map(|t| t.timestamp_millis()),
        exit_code: row.exit_code,
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn internal<E: std::fmt::Display>(err: E) -> Status {
    Status::internal(err.to_string())
}
