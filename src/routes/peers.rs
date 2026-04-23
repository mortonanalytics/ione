use axum::{
    extract::{Path, Query, State},
    response::{Json, Redirect},
};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    error::AppError,
    repos::{ConnectorRepo, PeerRepo, StreamRepo},
    services::peer::auto_create_connector_for_peer,
    state::AppState,
};

static PENDING_FEDERATIONS: Lazy<
    Mutex<HashMap<Uuid, crate::services::peer_oauth::PendingFederation>>,
> = Lazy::new(|| Mutex::new(HashMap::new()));

// ── Request types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePeerRequest {
    pub peer_url: String,
}

#[derive(Deserialize)]
pub(crate) struct AuthorizeAllowlistBody {
    #[serde(rename = "toolAllowlist")]
    pub tool_allowlist: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct CallbackQuery {
    pub code: String,
    pub state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyCreatePeerRequest {
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

/// POST /api/v1/peers — begin OAuth federation with a peer.
pub async fn create_peer(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    if req.get("peerUrl").is_none() {
        return create_legacy_peer(state, req).await;
    }

    let req: CreatePeerRequest = serde_json::from_value(req)
        .map_err(|e| AppError::BadRequest(format!("invalid peer request: {e}")))?;
    if req.peer_url.is_empty() {
        return Err(AppError::BadRequest("peerUrl is required".into()));
    }

    let issuer_id = ensure_local_peer_issuer(&state).await?;
    let peer = crate::services::peer::register_peer(
        &state.pool,
        &peer_name_from_url(&req.peer_url),
        &req.peer_url,
        issuer_id,
        json!({}),
    )
    .await
    .map_err(map_peer_registration_error)?;

    let begin = crate::services::peer_oauth::begin_federation(&state, peer.id, &req.peer_url)
        .await
        .map_err(AppError::Internal)?;

    PENDING_FEDERATIONS
        .lock()
        .await
        .insert(peer.id, begin.pending);

    Ok(Json(json!({
        "id": peer.id,
        "status": "pending_oauth",
        "authorizeUrl": begin.authorize_url,
    })))
}

async fn create_legacy_peer(state: AppState, req: Value) -> Result<Json<Value>, AppError> {
    let req: LegacyCreatePeerRequest = serde_json::from_value(req)
        .map_err(|e| AppError::BadRequest(format!("invalid peer request: {e}")))?;
    if req.name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if req.mcp_url.is_empty() {
        return Err(AppError::BadRequest("mcpUrl is required".into()));
    }

    let sharing_policy = req.sharing_policy.unwrap_or_else(|| json!({}));

    let peer = crate::services::peer::register_peer(
        &state.pool,
        &req.name,
        &req.mcp_url,
        req.issuer_id,
        sharing_policy,
    )
    .await
    .map_err(map_peer_registration_error)?;

    Ok(Json(
        serde_json::to_value(&peer).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub(crate) async fn callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, AppError> {
    let peer_id =
        Uuid::parse_str(&q.state).map_err(|_| AppError::BadRequest("invalid state".into()))?;
    let pending = PENDING_FEDERATIONS
        .lock()
        .await
        .remove(&peer_id)
        .ok_or_else(|| AppError::BadRequest("no pending federation for this state".into()))?;
    crate::services::peer_oauth::complete_callback(&state, &pending, &q.code)
        .await
        .map_err(AppError::Internal)?;
    let _manifest = fetch_manifest_over_mcp(&state, peer_id)
        .await
        .unwrap_or_else(|_| json!({ "tools": [] }));
    Ok(Redirect::to(&format!("/#/peers/{peer_id}")))
}

pub(crate) async fn get_manifest(
    State(state): State<AppState>,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let manifest = fetch_manifest_over_mcp(&state, peer_id)
        .await
        .unwrap_or_else(|_| json!({ "tools": [] }));
    Ok(Json(manifest))
}

pub(crate) async fn authorize_allowlist(
    State(state): State<AppState>,
    Path(peer_id): Path<Uuid>,
    Json(body): Json<AuthorizeAllowlistBody>,
) -> Result<Json<Value>, AppError> {
    let allowlist = Value::Array(body.tool_allowlist.into_iter().map(Value::String).collect());
    let peer_repo = PeerRepo::new(state.pool.clone());
    peer_repo
        .set_allowlist(peer_id, &allowlist)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "id": peer_id, "status": "active" })))
}

pub(crate) async fn delete_peer(
    State(state): State<AppState>,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let peer_repo = PeerRepo::new(state.pool.clone());
    peer_repo
        .set_status(peer_id, "revoked")
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "ok": true })))
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

async fn ensure_local_peer_issuer(state: &AppState) -> Result<Uuid, AppError> {
    let org_id: Uuid = sqlx::query_scalar("SELECT org_id FROM workspaces WHERE id = $1")
        .bind(state.default_workspace_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (org_id, issuer_url, audience) DO UPDATE
         SET jwks_uri = trust_issuers.jwks_uri
         RETURNING id",
    )
    .bind(org_id)
    .bind(&state.config.oauth_issuer)
    .bind("mcp")
    .bind("local-peer-federation")
    .bind(json!({ "sub": "sub" }))
    .fetch_one(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

fn peer_name_from_url(peer_url: &str) -> String {
    reqwest::Url::parse(peer_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| peer_url.to_owned())
}

fn map_peer_registration_error(e: anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("not found") || msg.contains("issuer_id") {
        AppError::BadRequest(msg)
    } else if msg.contains("unique") || msg.contains("duplicate") {
        AppError::BadRequest("peer URL already registered".into())
    } else {
        AppError::Internal(e)
    }
}

async fn fetch_manifest_over_mcp(_state: &AppState, _peer_id: Uuid) -> anyhow::Result<Value> {
    // TODO: real manifest fetch. T9.2 stores token hashes, so there is no bearer
    // token available for a tools/list MCP round trip yet.
    Ok(json!({ "tools": [] }))
}
