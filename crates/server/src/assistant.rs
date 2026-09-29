//! AI teaching assistant, backed by Anthropic's Managed Agents.
//!
//! Hermione is a thin relay: each course with the assistant enabled maps to a
//! persisted Agent (its `system` prompt, `skills`, and `mcp_servers` mirror the
//! teacher's dashboard config), and each student's conversation maps to a
//! Managed Agents session. The teacher configures and toggles the assistant per
//! course; courses without it work exactly as before.
//!
//! The whole feature is gated on `HERMIONE_ANTHROPIC_API_KEY`. When it's unset,
//! [`Assistant::enabled`] is false and every assistant route reports "disabled".

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Extension, Path, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use chrono::Utc;
use futures::StreamExt;
use hermione_entity::{assistant_conversations, assistant_messages, course_assistants, courses};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::auth::AuthCtx;
use crate::error::{ApiError, ApiResult};
use crate::http::{resolve_course, CourseCtx, CourseQuery, VerifiedStudent};
use crate::state::AppState;
use crate::text::non_blank;

/// Anthropic API base. Overridable so a proxy/gateway can be slotted in.
const API_BASE: &str = "https://api.anthropic.com";
/// Beta header that enables the Managed Agents endpoints.
const MANAGED_AGENTS_BETA: &str = "managed-agents-2026-04-01";
/// Shared environment name; environment names are unique per organization.
const ENVIRONMENT_NAME: &str = "hermione-assistant";

/// How long a single chat turn may run before we give up polling.
const TURN_TIMEOUT_SECS: u64 = 150;
/// How often we poll the session's event list while a turn is in flight.
const POLL_INTERVAL_MS: u64 = 1200;
/// Max history turns returned to the chat panel.
const HISTORY_LIMIT: u64 = 100;

// --- configuration value types (stored as JSON text on course_assistants) ----

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRef {
    /// "anthropic" (prebuilt, e.g. `xlsx`) or "custom" (a Skills API id).
    #[serde(rename = "type")]
    pub kind: String,
    pub skill_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub url: String,
}

fn parse_json_list<T: for<'de> Deserialize<'de>>(s: &str) -> Vec<T> {
    serde_json::from_str(s).unwrap_or_default()
}

// --- errors ------------------------------------------------------------------

/// Why a call to the Managed Agents API did not give an answer.
///
/// Kept apart from a plain string because the people who may see the failure
/// differ: the log gets all of it, a teacher who just changed the config gets
/// what Anthropic said about that request ([`Self::for_teacher`]), and a student
/// gets only that the assistant is unavailable.
#[derive(Debug, thiserror::Error)]
pub enum AssistantError {
    #[error("assistant not configured (HERMIONE_ANTHROPIC_API_KEY unset)")]
    NotConfigured,
    #[error("request to anthropic failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("anthropic answered {status}: {body}")]
    Upstream {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("unexpected answer from anthropic: {0}")]
    Malformed(String),
    #[error("session terminated")]
    Terminated,
    #[error("assistant timed out")]
    TimedOut,
}

impl AssistantError {
    /// What a teacher configuring the assistant may be told: for a request
    /// Anthropic rejected (a 4xx), its own explanation of what was wrong with
    /// it; otherwise only that the service is unavailable.
    pub fn for_teacher(&self) -> String {
        match self {
            Self::NotConfigured => self.to_string(),
            Self::Upstream { status, body } if status.is_client_error() => {
                serde_json::from_str::<Value>(body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("anthropic rejected the request ({status})"))
            }
            _ => "the assistant service is unavailable".to_string(),
        }
    }
}

/// What a student, or any caller that is not configuring the assistant, sees.
/// The detail goes to the log.
impl From<AssistantError> for ApiError {
    fn from(e: AssistantError) -> Self {
        tracing::warn!(error = %e, "assistant request failed");
        match e {
            AssistantError::NotConfigured => {
                ApiError::unavailable("the assistant is not configured on this server")
            }
            AssistantError::TimedOut => ApiError::bad_gateway("the assistant took too long"),
            _ => ApiError::bad_gateway("the assistant is unavailable right now"),
        }
    }
}

// --- the Managed Agents client ---------------------------------------------

/// Handle to Anthropic's Managed Agents API. Cloneable; lives in [`AppState`].
#[derive(Clone)]
pub struct Assistant {
    client: reqwest::Client,
    /// A second client with no overall timeout, for the long-lived SSE stream.
    stream_client: reqwest::Client,
    api_key: Option<String>,
    /// The shared cloud environment id, resolved lazily and cached.
    environment_id: Arc<RwLock<Option<String>>>,
}

impl Assistant {
    pub fn new(api_key: Option<String>, environment_id: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let stream_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self {
            client,
            stream_client,
            api_key,
            environment_id: Arc::new(RwLock::new(environment_id)),
        }
    }

    /// True when an API key is configured, i.e. the assistant can run at all.
    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    fn key(&self) -> Result<&str, AssistantError> {
        self.api_key.as_deref().ok_or(AssistantError::NotConfigured)
    }

    async fn api(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, AssistantError> {
        let key = self.key()?;
        let mut req = self
            .client
            .request(method, format!("{API_BASE}{path}"))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", MANAGED_AGENTS_BETA);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(AssistantError::Upstream { status, body: text });
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| AssistantError::Malformed(e.to_string()))
    }

    async fn get(&self, path: &str) -> Result<Value, AssistantError> {
        self.api(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, AssistantError> {
        self.api(reqwest::Method::POST, path, Some(body)).await
    }

    /// Resolves the shared cloud environment, creating it if absent. Cached for
    /// the process lifetime.
    async fn ensure_environment(&self) -> Result<String, AssistantError> {
        if let Some(id) = self.environment_id.read().await.clone() {
            return Ok(id);
        }
        let mut guard = self.environment_id.write().await;
        if let Some(id) = guard.clone() {
            return Ok(id);
        }
        // Try to create; on a name conflict, look the existing one up.
        let body = json!({
            "name": ENVIRONMENT_NAME,
            "config": { "type": "cloud", "networking": { "type": "unrestricted" } },
        });
        let id = match self.post("/v1/environments", body).await {
            Ok(v) => v.get("id").and_then(|i| i.as_str()).map(String::from),
            Err(AssistantError::Upstream { status, .. })
                if status == reqwest::StatusCode::CONFLICT =>
            {
                self.find_environment_by_name().await?
            }
            Err(e) => return Err(e),
        };
        let id = id
            .ok_or_else(|| AssistantError::Malformed("environment create returned no id".into()))?;
        *guard = Some(id.clone());
        Ok(id)
    }

    async fn find_environment_by_name(&self) -> Result<Option<String>, AssistantError> {
        let list = self.get("/v1/environments").await?;
        let id = list
            .get("data")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(ENVIRONMENT_NAME))
            .and_then(|e| e.get("id").and_then(|i| i.as_str()))
            .map(String::from);
        Ok(id)
    }

    /// Creates or updates the course's Agent from its config, returning the
    /// agent id and version.
    async fn sync_agent(
        &self,
        existing_agent_id: Option<&str>,
        course_name: &str,
        model: &str,
        system_prompt: &str,
        skills: &[SkillRef],
        mcp_servers: &[McpServer],
    ) -> Result<(String, String), AssistantError> {
        // Built-in toolset (bash/read/edit/web) plus one mcp_toolset per server,
        // so declared MCP servers are actually reachable by the agent.
        let mut tools = vec![json!({ "type": "agent_toolset_20260401" })];
        for s in mcp_servers {
            tools.push(json!({ "type": "mcp_toolset", "mcp_server_name": s.name }));
        }

        let mut body = json!({
            "name": format!("Hermione TA — {course_name}"),
            "model": model,
            "tools": tools,
        });
        let obj = body.as_object_mut().unwrap();
        if !system_prompt.trim().is_empty() {
            obj.insert("system".into(), json!(system_prompt));
        }
        if !skills.is_empty() {
            let skills_json: Vec<Value> = skills
                .iter()
                .map(|s| {
                    let mut m = json!({ "type": s.kind, "skill_id": s.skill_id });
                    if let Some(v) = &s.version {
                        m.as_object_mut()
                            .unwrap()
                            .insert("version".into(), json!(v));
                    }
                    m
                })
                .collect();
            obj.insert("skills".into(), json!(skills_json));
        }
        if !mcp_servers.is_empty() {
            let servers: Vec<Value> = mcp_servers
                .iter()
                .map(|s| json!({ "type": "url", "name": s.name, "url": s.url }))
                .collect();
            obj.insert("mcp_servers".into(), json!(servers));
        }

        let path = match existing_agent_id {
            Some(id) => format!("/v1/agents/{id}"), // update → new version
            None => "/v1/agents".to_string(),
        };
        let resp = self.post(&path, body).await?;
        let agent_id = existing_agent_id
            .map(String::from)
            .or_else(|| resp.get("id").and_then(|i| i.as_str()).map(String::from))
            .ok_or_else(|| AssistantError::Malformed("agent sync returned no id".into()))?;
        let version = version_string(&resp);
        Ok((agent_id, version))
    }

    /// Opens a fresh session bound to the given agent.
    async fn open_session(&self, agent_id: &str) -> Result<String, AssistantError> {
        let env = self.ensure_environment().await?;
        let resp = self
            .post(
                "/v1/sessions",
                json!({ "agent": agent_id, "environment_id": env }),
            )
            .await?;
        resp.get("id")
            .and_then(|i| i.as_str())
            .map(String::from)
            .ok_or_else(|| AssistantError::Malformed("session create returned no id".into()))
    }

    /// Runs one chat turn: sends the student's message and polls the session's
    /// events until it goes idle, accumulating the agent's reply text.
    async fn run_turn(&self, session_id: &str, text: &str) -> Result<String, AssistantError> {
        // Seed the seen-set with pre-existing events so we read only this turn's.
        let mut seen = self.list_event_ids(session_id).await?;

        self.post(
            &format!("/v1/sessions/{session_id}/events"),
            json!({ "events": [{ "type": "user.message", "content": [{ "type": "text", "text": text }] }] }),
        )
        .await?;

        let mut reply = String::new();
        let poll = async {
            loop {
                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
                let events = self
                    .get(&format!("/v1/sessions/{session_id}/events?limit=1000"))
                    .await?;
                let data = events.get("data").and_then(|d| d.as_array());
                for ev in data.into_iter().flatten() {
                    let id = ev.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if id.is_empty() || !seen.insert(id.to_string()) {
                        continue;
                    }
                    match ev.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                        "agent.message" => {
                            for block in ev
                                .get("content")
                                .and_then(|c| c.as_array())
                                .into_iter()
                                .flatten()
                            {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                        reply.push_str(t);
                                    }
                                }
                            }
                        }
                        // Idle marks turn completion unless the agent is blocked
                        // waiting on the client (which can't happen here — all
                        // tools are server-side and auto-approved).
                        "session.status_idle" => {
                            let kind = ev
                                .get("stop_reason")
                                .and_then(|s| s.get("type"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("end_turn");
                            if kind != "requires_action" {
                                return Ok::<(), AssistantError>(());
                            }
                        }
                        "session.status_terminated" => {
                            return Err(AssistantError::Terminated);
                        }
                        _ => {}
                    }
                }
            }
        };

        match tokio::time::timeout(Duration::from_secs(TURN_TIMEOUT_SECS), poll).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) if reply.is_empty() => return Err(AssistantError::TimedOut),
            Err(_) => {}
        }
        let reply = reply.trim().to_string();
        if reply.is_empty() {
            Ok("(the assistant finished without a textual reply)".to_string())
        } else {
            Ok(reply)
        }
    }

    async fn list_event_ids(&self, session_id: &str) -> Result<HashSet<String>, AssistantError> {
        let events = self
            .get(&format!("/v1/sessions/{session_id}/events?limit=1000"))
            .await?;
        Ok(events
            .get("data")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("id").and_then(|i| i.as_str()).map(String::from))
            .collect())
    }

    /// Opens the session's SSE event stream. Returns once response headers
    /// arrive, so the caller can send the user message into an already-open
    /// stream (the "stream-first" ordering Managed Agents requires).
    async fn open_event_stream(
        &self,
        session_id: &str,
    ) -> Result<reqwest::Response, AssistantError> {
        let key = self.key()?;
        let resp = self
            .stream_client
            .get(format!("{API_BASE}/v1/sessions/{session_id}/events/stream"))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", MANAGED_AGENTS_BETA)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AssistantError::Upstream { status, body });
        }
        Ok(resp)
    }

    /// Runs one chat turn over the SSE stream, forwarding each agent message and
    /// status to `tx` as it arrives, and returns the full accumulated reply.
    ///
    /// `Err` is reserved for failures *before* streaming begins (open/send), so
    /// the caller can safely retry on a fresh session; a mid-stream drop or
    /// timeout ends the turn with whatever text arrived so far.
    async fn run_streaming_turn(
        &self,
        session_id: &str,
        text: &str,
        tx: &mpsc::Sender<TurnEvent>,
    ) -> Result<String, AssistantError> {
        let resp = self.open_event_stream(session_id).await?;
        self.post(
            &format!("/v1/sessions/{session_id}/events"),
            json!({ "events": [{ "type": "user.message", "content": [{ "type": "text", "text": text }] }] }),
        )
        .await?;

        let mut reply = String::new();
        let mut buf = String::new();
        let mut bytes = resp.bytes_stream();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(TURN_TIMEOUT_SECS);

        'read: loop {
            // Stream end, transport error, or overall timeout all end the turn
            // with whatever we've gathered.
            let chunk = match tokio::time::timeout_at(deadline, bytes.next()).await {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break 'read,
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // SSE frames are line-oriented; we dispatch on each `data:` JSON's
            // `type` and ignore `event:`/keep-alive lines.
            while let Some(i) = buf.find('\n') {
                let line = buf[..i].trim_end().to_string();
                buf.drain(..=i);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "agent.message" => {
                        let mut msg = String::new();
                        for b in v
                            .get("content")
                            .and_then(|c| c.as_array())
                            .into_iter()
                            .flatten()
                        {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                    msg.push_str(t);
                                }
                            }
                        }
                        if !msg.is_empty() {
                            reply.push_str(&msg);
                            if tx.send(TurnEvent::Message(msg)).await.is_err() {
                                break 'read; // client disconnected
                            }
                        }
                    }
                    "agent.thinking" => {
                        let _ = tx.send(TurnEvent::Status("thinking…".into())).await;
                    }
                    "agent.tool_use" => {
                        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("a tool");
                        let _ = tx.send(TurnEvent::Status(format!("running {name}…"))).await;
                    }
                    "agent.mcp_tool_use" => {
                        let _ = tx
                            .send(TurnEvent::Status("using a connected tool…".into()))
                            .await;
                    }
                    "session.status_idle" => {
                        let kind = v
                            .get("stop_reason")
                            .and_then(|s| s.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("end_turn");
                        if kind != "requires_action" {
                            break 'read;
                        }
                    }
                    "session.status_terminated" => break 'read,
                    _ => {}
                }
            }
        }
        Ok(reply)
    }
}

/// An incremental update from a streaming turn, relayed to the client as SSE.
enum TurnEvent {
    /// A complete agent message (Managed Agents streams at message granularity).
    Message(String),
    /// A transient progress note ("thinking…", "running bash…").
    Status(String),
    Error(String),
    Done,
}

impl TurnEvent {
    fn into_event(self) -> Event {
        match self {
            TurnEvent::Message(t) => Event::default()
                .event("message")
                .data(json!({ "text": t }).to_string()),
            TurnEvent::Status(t) => Event::default()
                .event("status")
                .data(json!({ "text": t }).to_string()),
            TurnEvent::Error(t) => Event::default()
                .event("error")
                .data(json!({ "text": t }).to_string()),
            TurnEvent::Done => Event::default().event("done").data("{}"),
        }
    }
}

fn version_string(resp: &Value) -> String {
    match resp.get("version") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// --- DTOs ------------------------------------------------------------------

/// The teacher-facing config, returned by GET and accepted by PUT.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDto {
    pub enabled: bool,
    pub model: String,
    pub system_prompt: String,
    pub skills: Vec<SkillRef>,
    pub mcp_servers: Vec<McpServer>,
    /// Whether the server has an API key at all (assistant feature available).
    #[serde(default)]
    pub available: bool,
    /// Whether an Agent has been synced for this course.
    #[serde(default)]
    pub configured: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutConfig {
    pub course: Option<String>,
    pub enabled: bool,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillRef>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
}

// --- teacher handlers (dashboard) ------------------------------------------

/// GET /api/assistant?course=… — the course's assistant config.
pub async fn get_config(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> ApiResult {
    let course_id = resolve_course(&state, ctx, q.course).await?;

    let row = course_assistants::Entity::find_by_id(course_id)
        .one(&state.db)
        .await?;

    let dto = match row {
        Some(m) => ConfigDto {
            enabled: m.enabled,
            model: m.model,
            system_prompt: m.system_prompt,
            skills: parse_json_list(&m.skills),
            mcp_servers: parse_json_list(&m.mcp_servers),
            available: state.assistant.enabled(),
            configured: m.agent_id.is_some(),
        },
        None => ConfigDto {
            enabled: false,
            model: state.assistant_default_model.clone(),
            system_prompt: String::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            available: state.assistant.enabled(),
            configured: false,
        },
    };
    Ok(Json(dto).into_response())
}

/// PUT /api/assistant — upsert the config; syncs the Anthropic Agent when enabled.
pub async fn put_config(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<PutConfig>,
) -> ApiResult {
    let course_id = resolve_course(&state, ctx, body.course).await?;

    if body.enabled && !state.assistant.enabled() {
        return Err(ApiError::bad_request(
            "the assistant is not configured on this server (HERMIONE_ANTHROPIC_API_KEY is unset)",
        ));
    }

    let model = body
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| state.assistant_default_model.clone());
    let system_prompt = body.system_prompt.unwrap_or_default();

    let existing = course_assistants::Entity::find_by_id(course_id)
        .one(&state.db)
        .await?;

    // Sync (create/update) the Agent only when the assistant is enabled.
    let (mut agent_id, mut agent_version, mut environment_id) = match &existing {
        Some(m) => (
            m.agent_id.clone(),
            m.agent_version.clone(),
            m.environment_id.clone(),
        ),
        None => (None, None, None),
    };
    if body.enabled {
        let course_name = courses::Entity::find_by_id(course_id)
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .map(|c| c.name)
            .unwrap_or_else(|| "course".to_string());
        match state
            .assistant
            .sync_agent(
                agent_id.as_deref(),
                &course_name,
                &model,
                &system_prompt,
                &body.skills,
                &body.mcp_servers,
            )
            .await
        {
            Ok((id, version)) => {
                agent_id = Some(id);
                agent_version = Some(version);
                environment_id = state.assistant.environment_id.read().await.clone();
            }
            Err(e) => {
                tracing::warn!(error = %e, "agent sync failed");
                return Err(ApiError::bad_gateway(format!(
                    "agent sync failed: {}",
                    e.for_teacher()
                )));
            }
        }
    }

    let skills_text = serde_json::to_string(&body.skills).unwrap_or_else(|_| "[]".into());
    let mcp_text = serde_json::to_string(&body.mcp_servers).unwrap_or_else(|_| "[]".into());
    let upsert = ActiveModelFrom {
        course_id,
        enabled: body.enabled,
        model: model.clone(),
        system_prompt: system_prompt.clone(),
        skills: skills_text,
        mcp_servers: mcp_text,
        agent_id: agent_id.clone(),
        agent_version,
        environment_id,
        existed: existing.is_some(),
    };
    upsert.save(&state.db).await?;

    Ok(Json(ConfigDto {
        enabled: body.enabled,
        model,
        system_prompt,
        skills: body.skills,
        mcp_servers: body.mcp_servers,
        available: state.assistant.enabled(),
        configured: agent_id.is_some(),
    })
    .into_response())
}

/// Small helper to upsert a `course_assistants` row by primary key.
struct ActiveModelFrom {
    course_id: Uuid,
    enabled: bool,
    model: String,
    system_prompt: String,
    skills: String,
    mcp_servers: String,
    agent_id: Option<String>,
    agent_version: Option<String>,
    environment_id: Option<String>,
    existed: bool,
}

impl ActiveModelFrom {
    async fn save(self, db: &sea_orm::DatabaseConnection) -> Result<(), DbErr> {
        let active = course_assistants::ActiveModel {
            course_id: Set(self.course_id),
            enabled: Set(self.enabled),
            model: Set(self.model),
            system_prompt: Set(self.system_prompt),
            skills: Set(self.skills),
            mcp_servers: Set(self.mcp_servers),
            agent_id: Set(self.agent_id),
            agent_version: Set(self.agent_version),
            environment_id: Set(self.environment_id),
            updated_at: Set(Utc::now().into()),
        };
        if self.existed {
            course_assistants::Entity::update(active).exec(db).await?;
        } else {
            course_assistants::Entity::insert(active).exec(db).await?;
        }
        Ok(())
    }
}

// --- teacher transcripts (read student↔assistant conversations) -------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationSummary {
    id: String,
    student: String,
    message_count: u64,
    last_activity_unix_ms: i64,
    /// "student" or "assistant" — who spoke last.
    last_role: Option<String>,
    /// A short, single-line preview of the last message.
    preview: Option<String>,
}

/// GET /api/assistant/conversations?course=… — one row per student who has
/// chatted with the course assistant, newest activity first.
pub async fn list_conversations(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> ApiResult {
    let course_id = resolve_course(&state, ctx, q.course).await?;

    let conversations = assistant_conversations::Entity::find()
        .filter(assistant_conversations::Column::CourseId.eq(course_id))
        .order_by_desc(assistant_conversations::Column::UpdatedAt)
        .all(&state.db)
        .await?;

    // Two queries for the whole list, however many students there are: the
    // message count per conversation, and each conversation's last message. The
    // last message also fixes up `last_activity` for older rows whose
    // `updated_at` only tracked the session, not the transcript.
    let ids: Vec<Uuid> = conversations.iter().map(|c| c.id).collect();
    let (counts, mut lasts) = if ids.is_empty() {
        Default::default()
    } else {
        let counts: HashMap<Uuid, u64> = assistant_messages::Entity::find()
            .select_only()
            .column(assistant_messages::Column::ConversationId)
            .column_as(assistant_messages::Column::Id.count(), "n")
            .filter(assistant_messages::Column::ConversationId.is_in(ids.clone()))
            .group_by(assistant_messages::Column::ConversationId)
            .into_tuple::<(Uuid, i64)>()
            .all(&state.db)
            .await?
            .into_iter()
            .map(|(id, n)| (id, n as u64))
            .collect();
        let lasts: HashMap<Uuid, assistant_messages::Model> = assistant_messages::Entity::find()
            .filter(assistant_messages::Column::ConversationId.is_in(ids))
            .distinct_on([assistant_messages::Column::ConversationId])
            .order_by_asc(assistant_messages::Column::ConversationId)
            .order_by_desc(assistant_messages::Column::Id)
            .all(&state.db)
            .await?
            .into_iter()
            .map(|m| (m.conversation_id, m))
            .collect();
        (counts, lasts)
    };

    let out: Vec<ConversationSummary> = conversations
        .into_iter()
        .map(|c| {
            let last = lasts.remove(&c.id);
            ConversationSummary {
                id: c.id.to_string(),
                student: c.student,
                message_count: counts.get(&c.id).copied().unwrap_or(0),
                last_activity_unix_ms: last
                    .as_ref()
                    .map_or_else(|| c.updated_at, |m| m.created_at)
                    .timestamp_millis(),
                last_role: last.as_ref().map(|m| m.role.clone()),
                preview: last.map(|m| preview(&m.body)),
            }
        })
        .collect();
    Ok(Json(out).into_response())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationDetail {
    id: String,
    student: String,
    messages: Vec<HistoryMessage>,
}

/// GET /api/assistant/conversations/{id}/messages?course=… — the full
/// transcript of one student's conversation, for the teacher view.
pub async fn conversation_messages(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(id): Path<String>,
    Query(q): Query<CourseQuery>,
) -> ApiResult {
    let course_id = resolve_course(&state, ctx, q.course).await?;
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::bad_request("invalid conversation id"))?;

    let conversation = assistant_conversations::Entity::find_by_id(id)
        .one(&state.db)
        .await?;
    // Scope to the resolved course: a conversation from another course is not
    // visible here even with a valid id.
    let Some(conversation) = conversation.filter(|c| c.course_id == course_id) else {
        return Err(ApiError::not_found("no such conversation"));
    };

    let messages = assistant_messages::Entity::find()
        .filter(assistant_messages::Column::ConversationId.eq(conversation.id))
        .order_by_asc(assistant_messages::Column::Id)
        .all(&state.db)
        .await?
        .into_iter()
        .map(HistoryMessage::from)
        .collect();

    Ok(Json(ConversationDetail {
        id: conversation.id.to_string(),
        student: conversation.student,
        messages,
    })
    .into_response())
}

/// Collapses a message body to a short single-line preview.
fn preview(body: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        let truncated: String = flat.chars().take(120).collect();
        format!("{truncated}…")
    } else {
        flat
    }
}

// --- student handlers (extension, enrollment-token-scoped) ------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    enabled: bool,
}

/// GET /api/assistant/status — whether the assistant is live for this course,
/// so the extension can show or hide its chat panel.
pub async fn status(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
) -> ApiResult {
    let enabled = state.assistant.enabled() && assistant_live(&state, course_id).await;
    Ok(Json(StatusDto { enabled }).into_response())
}

/// True when the course has an enabled, agent-synced assistant.
async fn assistant_live(state: &AppState, course_id: Uuid) -> bool {
    matches!(
        course_assistants::Entity::find_by_id(course_id).one(&state.db).await,
        Ok(Some(m)) if m.enabled && m.agent_id.is_some()
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub message: String,
    pub student: Option<String>,
    /// Optional editor context to ground the answer.
    pub file: Option<String>,
    pub language: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatReply {
    reply: String,
}

/// A validated, ready-to-run turn: the conversation, the synced agent, and the
/// context-grounded prompt. The student's message is already persisted.
struct PreparedTurn {
    conversation: assistant_conversations::Model,
    agent_id: String,
    prompt: String,
}

/// Which student a request is for: a verified identity (when the deployment
/// enforces one) wins over the name the request asserts. `None` when neither is
/// usable.
fn asserted_student(verified: Option<String>, claimed: Option<String>) -> Option<String> {
    non_blank(verified.or(claimed).as_deref())
}

/// Shared validation for both chat endpoints: checks the assistant is live,
/// resolves the student and conversation, persists the question, and builds the
/// context-grounded prompt.
async fn prepare_turn(
    state: &AppState,
    course_id: Uuid,
    verified: Option<String>,
    body: ChatRequest,
) -> ApiResult<PreparedTurn> {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::bad_request("empty message"));
    }
    // A verified identity (when enforced) wins over the self-asserted one.
    let student = asserted_student(verified, body.student)
        .ok_or_else(|| ApiError::bad_request("missing student"))?;

    let not_enabled = || ApiError::not_found("assistant not enabled for this course");
    let row = course_assistants::Entity::find_by_id(course_id)
        .one(&state.db)
        .await?
        .ok_or_else(not_enabled)?;
    let agent_id = match row.agent_id {
        Some(id) if row.enabled && state.assistant.enabled() => id,
        _ => return Err(not_enabled()),
    };

    let conversation = find_or_create_conversation(state, course_id, &student).await?;
    insert_message(state, conversation.id, "student", &message).await?;

    // Ground the turn with light editor context, if provided.
    let prompt = match (&body.file, &body.language) {
        (Some(f), Some(l)) if !f.is_empty() => {
            format!("[Student is editing `{f}` ({l})]\n\n{message}")
        }
        (Some(f), _) if !f.is_empty() => format!("[Student is editing `{f}`]\n\n{message}"),
        _ => message,
    };

    Ok(PreparedTurn {
        conversation,
        agent_id,
        prompt,
    })
}

/// POST /api/assistant/chat — one turn with the course assistant (non-streaming).
pub async fn chat(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Json(body): Json<ChatRequest>,
) -> ApiResult {
    let prepared = prepare_turn(&state, course_id, verified, body).await?;

    let reply = run_with_session(
        &state,
        &prepared.conversation,
        &prepared.agent_id,
        &prepared.prompt,
    )
    .await?;
    insert_message(&state, prepared.conversation.id, "assistant", &reply).await?;

    Ok(Json(ChatReply { reply }).into_response())
}

/// POST /api/assistant/chat/stream — one turn, relayed to the client as SSE.
/// Managed Agents streams at message granularity, so each `message` event is a
/// complete agent message; `status` events surface progress between tool calls.
pub async fn chat_stream(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Json(body): Json<ChatRequest>,
) -> ApiResult {
    let prepared = prepare_turn(&state, course_id, verified, body).await?;

    let (tx, rx) = mpsc::channel::<TurnEvent>(32);
    let st = state.clone();
    tokio::spawn(async move {
        let assistant = st.assistant.clone();
        let PreparedTurn {
            conversation,
            agent_id,
            prompt,
        } = prepared;

        // Ensure a session, streaming the turn — re-opening once if it's stale.
        let session_id = match &conversation.session_id {
            Some(s) => s.clone(),
            None => match assistant.open_session(&agent_id).await {
                Ok(s) => {
                    let _ = set_session(&st, conversation.id, &s).await;
                    s
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not open an assistant session");
                    let _ = tx
                        .send(TurnEvent::Error("could not start a session".into()))
                        .await;
                    let _ = tx.send(TurnEvent::Done).await;
                    return;
                }
            },
        };

        let mut reply = match assistant
            .run_streaming_turn(&session_id, &prompt, &tx)
            .await
        {
            Ok(r) => r,
            // Open/send failed (e.g. session expired) — retry on a fresh one.
            Err(_) => match assistant.open_session(&agent_id).await {
                Ok(fresh) => {
                    let _ = set_session(&st, conversation.id, &fresh).await;
                    assistant
                        .run_streaming_turn(&fresh, &prompt, &tx)
                        .await
                        .unwrap_or_default()
                }
                Err(e) => {
                    tracing::warn!(error = %e, "assistant unavailable");
                    let _ = tx
                        .send(TurnEvent::Error("assistant unavailable".into()))
                        .await;
                    String::new()
                }
            },
        };

        reply = reply.trim().to_string();
        if reply.is_empty() {
            let _ = tx
                .send(TurnEvent::Error(
                    "The assistant didn't return a response.".into(),
                ))
                .await;
        } else {
            let _ = insert_message(&st, conversation.id, "assistant", &reply).await;
        }
        let _ = tx.send(TurnEvent::Done).await;
    });

    let stream = ReceiverStream::new(rx).map(|ev| Ok::<Event, Infallible>(ev.into_event()));
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Runs a turn, transparently opening a session (or re-opening a stale one).
async fn run_with_session(
    state: &AppState,
    conversation: &assistant_conversations::Model,
    agent_id: &str,
    prompt: &str,
) -> ApiResult<String> {
    let open = || async {
        let session = state.assistant.open_session(agent_id).await?;
        set_session(state, conversation.id, &session).await?;
        Ok::<_, ApiError>(session)
    };
    let session_id = match &conversation.session_id {
        Some(s) => s.clone(),
        None => open().await?,
    };

    match state.assistant.run_turn(&session_id, prompt).await {
        Ok(reply) => Ok(reply),
        // The session may have terminated/expired — open a fresh one and retry.
        Err(_) => {
            let fresh = open().await?;
            Ok(state.assistant.run_turn(&fresh, prompt).await?)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryMessage {
    role: String,
    body: String,
    created_at_unix_ms: i64,
}

impl From<assistant_messages::Model> for HistoryMessage {
    fn from(m: assistant_messages::Model) -> Self {
        HistoryMessage {
            role: m.role,
            body: m.body,
            created_at_unix_ms: m.created_at.timestamp_millis(),
        }
    }
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    student: Option<String>,
}

/// GET /api/assistant/history?student=… — this student's prior turns.
pub async fn history(
    State(state): State<AppState>,
    Extension(CourseCtx(course_id)): Extension<CourseCtx>,
    Extension(VerifiedStudent(verified)): Extension<VerifiedStudent>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult {
    let Some(student) = asserted_student(verified, q.student) else {
        return Ok(Json(Vec::<HistoryMessage>::new()).into_response());
    };

    let conversation = assistant_conversations::Entity::find()
        .filter(assistant_conversations::Column::CourseId.eq(course_id))
        .filter(assistant_conversations::Column::Student.eq(&student))
        .one(&state.db)
        .await?;
    let Some(conversation) = conversation else {
        return Ok(Json(Vec::<HistoryMessage>::new()).into_response());
    };

    let out: Vec<HistoryMessage> = assistant_messages::Entity::find()
        .filter(assistant_messages::Column::ConversationId.eq(conversation.id))
        .order_by_asc(assistant_messages::Column::Id)
        .limit(HISTORY_LIMIT)
        .all(&state.db)
        .await?
        .into_iter()
        .map(HistoryMessage::from)
        .collect();
    Ok(Json(out).into_response())
}

// --- conversation persistence ----------------------------------------------

async fn find_or_create_conversation(
    state: &AppState,
    course_id: Uuid,
    student: &str,
) -> Result<assistant_conversations::Model, DbErr> {
    if let Some(c) = find_conversation(state, course_id, student).await? {
        return Ok(c);
    }
    let now = Utc::now();
    let model = assistant_conversations::ActiveModel {
        id: Set(Uuid::new_v4()),
        course_id: Set(course_id),
        student: Set(student.to_string()),
        session_id: Set(None),
        created_at: Set(now.into()),
        updated_at: Set(now.into()),
    };
    match assistant_conversations::Entity::insert(model)
        .exec_with_returning(&state.db)
        .await
    {
        Ok(created) => Ok(created),
        // A double-submit raced us to the (course, student) row: use theirs.
        Err(e) => find_conversation(state, course_id, student).await?.ok_or(e),
    }
}

async fn find_conversation(
    state: &AppState,
    course_id: Uuid,
    student: &str,
) -> Result<Option<assistant_conversations::Model>, DbErr> {
    assistant_conversations::Entity::find()
        .filter(assistant_conversations::Column::CourseId.eq(course_id))
        .filter(assistant_conversations::Column::Student.eq(student))
        .one(&state.db)
        .await
}

async fn set_session(
    state: &AppState,
    conversation_id: Uuid,
    session_id: &str,
) -> Result<(), DbErr> {
    let model = assistant_conversations::ActiveModel {
        id: Set(conversation_id),
        session_id: Set(Some(session_id.to_string())),
        updated_at: Set(Utc::now().into()),
        ..Default::default()
    };
    assistant_conversations::Entity::update(model)
        .exec(&state.db)
        .await?;
    Ok(())
}

async fn insert_message(
    state: &AppState,
    conversation_id: Uuid,
    role: &str,
    body: &str,
) -> Result<(), DbErr> {
    let model = assistant_messages::ActiveModel {
        conversation_id: Set(conversation_id),
        role: Set(role.to_string()),
        body: Set(body.to_string()),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    };
    assistant_messages::Entity::insert(model)
        .exec(&state.db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    fn upstream(status: u16, body: &str) -> AssistantError {
        AssistantError::Upstream {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            body: body.to_string(),
        }
    }

    async fn shown_to_caller(e: AssistantError) -> (u16, String) {
        let resp = ApiError::from(e).into_response();
        let status = resp.status().as_u16();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn a_student_never_sees_what_anthropic_said() {
        let e = upstream(
            400,
            r#"{"error":{"message":"model 'x' not found for org acme"}}"#,
        );
        let (status, body) = shown_to_caller(e).await;
        assert_eq!(status, 502);
        assert!(
            !body.contains("acme") && !body.contains("anthropic"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn not_configured_and_slow_are_told_apart() {
        assert_eq!(shown_to_caller(AssistantError::NotConfigured).await.0, 503);
        assert_eq!(shown_to_caller(AssistantError::TimedOut).await.0, 502);
    }

    #[test]
    fn a_teacher_hears_why_anthropic_rejected_their_config() {
        let e = upstream(400, r#"{"error":{"message":"unknown model 'sonet'"}}"#);
        assert_eq!(e.for_teacher(), "unknown model 'sonet'");
        // An unparseable rejection still says it was a rejection, not the body.
        assert_eq!(
            upstream(422, "<html>oops</html>").for_teacher(),
            "anthropic rejected the request (422 Unprocessable Entity)"
        );
    }

    #[test]
    fn a_teacher_is_not_told_about_our_or_their_outages() {
        assert_eq!(
            upstream(500, "internal secret detail").for_teacher(),
            "the assistant service is unavailable"
        );
        assert_eq!(
            AssistantError::Malformed("bad json at byte 3".into()).for_teacher(),
            "the assistant service is unavailable"
        );
    }
}
