use axum::{
    extract::{Path, Query, State},
    response::{Json, Redirect},
    Extension,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    repos::{ConnectorRepo, PeerRepo, StreamRepo},
    services::peer::auto_create_connector_for_peer,
    state::AppState,
};

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
pub async fn list_peers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Value>, AppError> {
    let repo = PeerRepo::new(state.pool.clone());
    let items = repo
        .list_for_org(ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/v1/peers — begin OAuth federation with a peer.
pub async fn create_peer(
    State(state): State<AppState>,
    Extension(_ctx): Extension<AuthContext>,
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

    let begin =
        crate::services::peer_oauth::begin_federation(&state, peer.id, &req.peer_url).await?;

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
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "DELETE FROM peer_oauth_pending
         WHERE nonce = $1 AND expires_at > now()
         RETURNING peer_id, code_verifier",
    )
    .bind(&q.state)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let (peer_id, code_verifier) =
        row.ok_or_else(|| AppError::BadRequest("invalid or expired state".into()))?;
    let peer = PeerRepo::new(state.pool.clone())
        .get(peer_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("peer not found".into()))?;
    let disc_value = crate::util::safe_http::fetch_public_metadata(
        &format!("{}/.well-known/oauth-authorization-server", peer.mcp_url),
        64_000,
        std::time::Duration::from_secs(5),
    )
    .await
    .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    let discovery: crate::services::peer_oauth::PeerDiscovery = serde_json::from_value(disc_value)
        .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    let pending = crate::services::peer_oauth::PendingFederation {
        peer_id,
        peer_url: peer.mcp_url.clone(),
        discovery,
        code_verifier,
        code_challenge: String::new(),
        client_id: peer.oauth_client_id.unwrap_or_default(),
        redirect_uri: format!("{}/api/v1/peers/callback", state.config.oauth_issuer),
        nonce: q.state.clone(),
    };
    crate::services::peer_oauth::complete_callback(&state, &pending, &q.code)
        .await
        .map_err(AppError::Internal)?;
    let _ = fetch_manifest_over_mcp(&state, peer_id).await;
    Ok(Redirect::to(&format!("/#/peers/{peer_id}")))
}

pub(crate) async fn get_manifest(
    State(state): State<AppState>,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let manifest = fetch_manifest_over_mcp(&state, peer_id)
        .await
        .map_err(AppError::Internal)?;
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

async fn fetch_manifest_over_mcp(state: &AppState, peer_id: Uuid) -> anyhow::Result<Value> {
    let peer_repo = PeerRepo::new(state.pool.clone());
    let peer = peer_repo
        .get(peer_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("peer {peer_id} not found"))?;

    if peer.status != crate::models::PeerStatus::PendingAllowlist {
        anyhow::bail!("peer is not pending allowlist");
    }

    let ciphertext = peer
        .access_token_ciphertext
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("peer access token is unavailable"))?;
    let access_token = crate::util::token_crypto::decrypt_token(ciphertext)?;
    let endpoint = peer.mcp_url.trim_end_matches('/');
    let resp: Value = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?
        .post(format!("{endpoint}/mcp"))
        .bearer_auth(access_token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let tools = resp
        .get("result")
        .and_then(|result| result.get("tools"))
        .cloned()
        .unwrap_or_else(|| json!([]));

    Ok(json!({ "tools": tools }))
}
