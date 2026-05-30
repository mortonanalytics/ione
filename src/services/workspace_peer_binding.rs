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
    let body: Value = crate::services::peer_tokens::send_mcp_request(
        &state.pool,
        &state.http,
        peer,
        endpoint,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": "whoami://" }
        }),
    )
    .await?
    .error_for_status()?
    .json()
    .await?;
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
