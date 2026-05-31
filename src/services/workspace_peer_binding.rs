use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    models::{Peer, WorkspacePeerBinding},
    repos::{PeerRepo, WorkspacePeerBindingRepo},
    state::AppState,
};

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct WhoamiResponse {
    pub peer_id: Option<String>,
    pub foreign_tenant_id: String,
    pub foreign_tenant_name: Option<String>,
    pub foreign_workspace_id: Option<String>,
    pub foreign_user_id: Option<String>,
    pub foreign_user_email: Option<String>,
    #[serde(default)]
    pub foreign_roles: Vec<String>,
}

pub async fn fetch_whoami(state: &AppState, peer: &Peer) -> anyhow::Result<WhoamiResponse> {
    let endpoint = peer.mcp_url.trim_end_matches('/');
    let request_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "resources/read",
        "params": { "uri": "whoami://" }
    });
    let body = match fetch_whoami_body(state, peer, endpoint, &request_body, None).await {
        Ok(body) => body,
        Err(e) if looks_like_missing_session(&e) => {
            let session_id = initialize_peer_session(state, peer, endpoint).await?;
            fetch_whoami_body(state, peer, endpoint, &request_body, Some(&session_id)).await?
        }
        Err(e) => return Err(e),
    };
    if let Some(err) = body.get("error").filter(|err| !err.is_null()) {
        anyhow::bail!("peer MCP error: {}", err);
    }

    let text = body
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("whoami response missing result.contents[0].text"))?;
    let whoami: WhoamiResponse = serde_json::from_str(text)?;
    if whoami.foreign_tenant_id.is_empty() {
        anyhow::bail!("whoami response missing foreign_tenant_id");
    }
    Ok(whoami)
}

async fn fetch_whoami_body(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
    mcp_session_id: Option<&str>,
) -> anyhow::Result<Value> {
    let body: Value = crate::services::peer_tokens::send_mcp_request_with_session(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        body,
        mcp_session_id,
    )
    .await?
    .error_for_status()?
    .json()
    .await?;
    if let Some(error) = body.get("error").filter(|error| !error.is_null()) {
        anyhow::bail!("peer MCP error: {}", error);
    }
    Ok(body)
}

async fn initialize_peer_session(
    state: &AppState,
    peer: &Peer,
    endpoint: &str,
) -> anyhow::Result<String> {
    let resp = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} }
        }),
    )
    .await?;
    let header_session = resp
        .headers()
        .get("MCP-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body: Value = resp.error_for_status()?.json().await?;
    header_session
        .or_else(|| {
            body.get("result")
                .and_then(|result| result.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow::anyhow!("peer initialize did not return a session id"))
}

fn looks_like_missing_session(error: &anyhow::Error) -> bool {
    let msg = error.to_string().to_ascii_lowercase();
    msg.contains("mcp-session-id") || msg.contains("session not found")
}

pub async fn bind_on_subscribe(
    state: &AppState,
    workspace_id: Uuid,
    peer: &Peer,
) -> anyhow::Result<WorkspacePeerBinding> {
    let whoami = tokio::time::timeout(Duration::from_secs(3), fetch_whoami(state, peer))
        .await
        .ok()
        .and_then(Result::ok);
    WorkspacePeerBindingRepo::new(state.pool.clone())
        .upsert_from_subscribe(workspace_id, peer.id, whoami.as_ref())
        .await
}

pub enum RefreshError {
    NotFound,
    PeerGone,
    Unreachable(String),
    Conflict { old: String, new: String },
    Db(anyhow::Error),
}

pub async fn refresh_binding(
    state: &AppState,
    binding_id: Uuid,
    org_id: Uuid,
) -> Result<WorkspacePeerBinding, RefreshError> {
    let binding_repo = WorkspacePeerBindingRepo::new(state.pool.clone());
    let binding = binding_repo
        .get_by_id_org_scoped(binding_id, org_id)
        .await
        .map_err(RefreshError::Db)?
        .ok_or(RefreshError::NotFound)?;
    let peer = PeerRepo::new(state.pool.clone())
        .get(binding.peer_id)
        .await
        .map_err(RefreshError::Db)?
        .ok_or(RefreshError::PeerGone)?;

    let whoami = fetch_whoami(state, &peer)
        .await
        .map_err(|e| RefreshError::Unreachable(e.to_string()))?;
    let old = binding.foreign_tenant_id.clone();
    let result = binding_repo
        .apply_whoami_refresh(binding_id, org_id, &whoami)
        .await
        .map_err(RefreshError::Db)?
        .ok_or(RefreshError::NotFound)?;

    if result.1 {
        return Err(RefreshError::Conflict {
            old,
            new: whoami.foreign_tenant_id,
        });
    }

    Ok(result.0)
}
