use std::{sync::Arc, time::Duration};

use dashmap::DashMap;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{sync::watch, task::JoinHandle};
use uuid::Uuid;

use crate::{repos::PeerRepo, state::AppState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Disconnected,
    Connecting,
    Live,
    Error(String),
}

pub struct PeerSessionHandle {
    pub state_tx: watch::Sender<SessionState>,
    pub task: JoinHandle<()>,
}

pub struct PeerSessionRegistry {
    tasks: DashMap<Uuid, PeerSessionHandle>,
    limit: Arc<tokio::sync::Semaphore>,
}

impl Default for PeerSessionRegistry {
    fn default() -> Self {
        Self::new(20)
    }
}

impl PeerSessionRegistry {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            tasks: DashMap::new(),
            limit: Arc::new(tokio::sync::Semaphore::new(max_sessions.max(1))),
        }
    }

    pub fn snapshot(&self, peer_id: Uuid) -> SessionState {
        self.tasks
            .get(&peer_id)
            .map(|entry| entry.state_tx.borrow().clone())
            .unwrap_or(SessionState::Disconnected)
    }

    pub fn reconnect(&self, state: AppState, peer_id: Uuid) {
        self.stop(peer_id);
        self.start(state, peer_id);
    }

    pub fn start(&self, state: AppState, peer_id: Uuid) {
        if self.tasks.contains_key(&peer_id) {
            return;
        }
        let (state_tx, _state_rx) = watch::channel(SessionState::Connecting);
        let tx = state_tx.clone();
        let limit = self.limit.clone();
        let task = tokio::spawn(async move {
            let _permit = match limit.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            };
            run_session_task(state, peer_id, tx).await;
        });
        self.tasks
            .insert(peer_id, PeerSessionHandle { state_tx, task });
    }

    pub fn stop(&self, peer_id: Uuid) {
        if let Some((_, handle)) = self.tasks.remove(&peer_id) {
            handle.task.abort();
        }
    }
}

async fn run_session_task(state: AppState, peer_id: Uuid, state_tx: watch::Sender<SessionState>) {
    let idle_secs = std::env::var("IONE_PEER_SSE_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(90);
    let mut backoff = Duration::from_secs(1);
    loop {
        let _ = state_tx.send(SessionState::Connecting);
        let _ = PeerRepo::new(state.pool.clone())
            .set_session_status(peer_id, "connecting", None)
            .await;
        match connect_and_read(&state, peer_id, Duration::from_secs(idle_secs), &state_tx).await {
            Ok(()) => {
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                let message = e.to_string();
                let _ = state_tx.send(SessionState::Error(message.clone()));
                let _ = PeerRepo::new(state.pool.clone())
                    .set_session_status(peer_id, "error", Some(&message))
                    .await;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(300));
            }
        }
    }
}

async fn connect_and_read(
    state: &AppState,
    peer_id: Uuid,
    idle_timeout: Duration,
    state_tx: &watch::Sender<SessionState>,
) -> anyhow::Result<()> {
    let peer = PeerRepo::new(state.pool.clone())
        .get(peer_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("peer not found"))?;
    let endpoint = peer.mcp_url.trim_end_matches('/').to_string();
    let init = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        &peer,
        &endpoint,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25" }
        }),
    )
    .await?
    .error_for_status()?;
    let header_session_id = init
        .headers()
        .get("MCP-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let init_json: Value = init.json().await?;
    let session_id = header_session_id
        .or_else(|| {
            init_json
                .get("result")
                .and_then(|result| result.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("peer initialize did not return a session id"))?;
    let token =
        crate::services::peer_tokens::resolve_access_token_locked(state, &peer).await?;
    let mut request = state
        .http
        .get(&endpoint)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header("MCP-Session-Id", session_id)
        .header("MCP-Protocol-Version", "2025-11-25");
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?.error_for_status()?;
    // The registry watch state feeds the browser badge; the DB fields give the
    // operator history across restarts.
    let _ = PeerRepo::new(state.pool.clone())
        .set_session_status(peer_id, "live", None)
        .await;
    let _ = state_tx.send(SessionState::Live);

    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    loop {
        let next = tokio::time::timeout(idle_timeout, stream.next()).await;
        let Some(chunk) = next.map_err(|_| anyhow::anyhow!("peer SSE idle timeout"))? else {
            anyhow::bail!("peer SSE stream ended");
        };
        let chunk = chunk?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = pending.find('\n') {
            let line = pending[..idx].trim_end_matches('\r').to_string();
            pending = pending[idx + 1..].to_string();
            handle_sse_line(state, peer_id, &line).await?;
        }
    }
}

async fn handle_sse_line(state: &AppState, peer_id: Uuid, line: &str) -> anyhow::Result<()> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(data)?;
    if value.get("method").is_some() {
        crate::services::federation::dispatch_notification(state, peer_id, value).await?;
    }
    Ok(())
}
