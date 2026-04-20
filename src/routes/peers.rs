use axum::{
    extract::{Path, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    repos::{ConnectorRepo, PeerRepo, StreamRepo},
    services::peer::{auto_create_connector_for_peer, register_peer},
    state::AppState,
};

// ── Request types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePeerRequest {
    pub name: String,
    pub mcp_url: String,
    pub issuer_id: Uuid,
    pub sharing_policy: Option<Value>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// GET /api/v1/peers — list all known peers.
pub async fn list_peers(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let repo = PeerRepo::new(state.pool.clone());
    let items = repo.list().await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/v1/peers — register a new peer.
pub async fn create_peer(
    State(state): State<AppState>,
    Json(req): Json<CreatePeerRequest>,
) -> Result<Json<Value>, AppError> {
    if req.name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if req.mcp_url.is_empty() {
        return Err(AppError::BadRequest("mcpUrl is required".into()));
    }

    let sharing_policy = req.sharing_policy.unwrap_or_else(|| json!({}));

    let peer = register_peer(
        &state.pool,
        &req.name,
        &req.mcp_url,
        req.issuer_id,
        sharing_policy,
    )
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") || msg.contains("issuer_id") {
            AppError::BadRequest(msg)
        } else if msg.contains("unique") || msg.contains("duplicate") {
            AppError::BadRequest(format!("mcp_url already registered: {}", req.mcp_url))
        } else {
            AppError::Internal(e)
        }
    })?;

    Ok(Json(
        serde_json::to_value(&peer).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

/// POST /api/v1/workspaces/:id/peers/:peerId/subscribe — subscribe a workspace to a peer.
/// Creates the mcp_client connector in the workspace and triggers a first poll.
pub async fn subscribe_peer(
    State(state): State<AppState>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let peer_repo = PeerRepo::new(state.pool.clone());
    let peer = peer_repo
        .get(peer_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("peer {} not found", peer_id)))?;

    let connector = auto_create_connector_for_peer(&state.pool, workspace_id, &peer)
        .await
        .map_err(AppError::Internal)?;

    // Trigger first poll: create default streams for this connector.
    trigger_first_poll(&state, connector.id);

    Ok(Json(
        serde_json::to_value(&connector).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

/// Spawn a background task to create streams and poll them for the first time.
fn trigger_first_poll(state: &AppState, connector_id: Uuid) {
    let pool = state.pool.clone();
    tokio::spawn(async move {
        if let Err(e) = first_poll_connector(&pool, connector_id).await {
            tracing::warn!(connector_id = %connector_id, error = %e, "first poll failed");
        }
    });
}

async fn first_poll_connector(pool: &sqlx::PgPool, connector_id: Uuid) -> anyhow::Result<()> {
    use crate::connectors::build_from_row;
    use crate::repos::StreamEventRepo;

    let connector_repo = ConnectorRepo::new(pool.clone());
    let stream_repo = StreamRepo::new(pool.clone());
    let event_repo = StreamEventRepo::new(pool.clone());

    let connector = connector_repo
        .get(connector_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("connector {} not found", connector_id))?;

    let impl_ = build_from_row(&connector)?;
    let descriptors = impl_.default_streams().await?;

    for desc in descriptors {
        let stream = stream_repo
            .upsert_named(connector_id, &desc.name, desc.schema)
            .await?;

        let poll_result = impl_.poll(&desc.name, None).await?;
        for event in poll_result.events {
            event_repo
                .insert_if_absent(stream.id, event.payload, event.observed_at)
                .await?;
        }
    }

    Ok(())
}
