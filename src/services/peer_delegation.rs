//! Brokered delegated tokens scoped to one (workspace, peer) — issue #12.
//!
//! The operator authorizes IONe against a peer's OAuth authorization server
//! once, for one workspace. IONe stores the resulting access/refresh pair
//! encrypted at rest and presents it as `Authorization: Bearer <token>` on every
//! outbound MCP request made in that workspace's scope — so a later operator
//! session drives peer tools with no second login at the peer.
//!
//! Relationship to the peer-global grant (`peers.access_token_ciphertext`):
//! this is strictly more specific. The precedence chain that consumes it is
//! documented on `services::peer_tokens::resolve_access_token`.
//!
//! The wire contract is unchanged: `Authorization: Bearer <token>`, frozen in
//! md/design/app-integration-contract-v1.md. A peer cannot tell which of the
//! four credential modes IONe is in.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::Peer,
    repos::{PeerRepo, WorkspacePeerDelegationRepo},
    services::{peer_oauth::PeerDiscovery, IdentityAuditWriter, IdentityEvent},
    state::AppState,
    util::token_crypto,
};

/// Refresh a delegated token this many seconds before it actually expires, so a
/// token does not go stale mid-flight. Matches `peer_tokens::REFRESH_SKEW_SECONDS`.
const REFRESH_SKEW_SECONDS: i64 = 60;

/// Where the peer's authorization server sends the operator back.
fn redirect_uri(state: &AppState) -> String {
    format!("{}/auth/broker/peer-callback", state.config.oauth_issuer)
}

/// Begin the delegation dance for (workspace, peer). Returns the peer
/// authorization-server URL the operator must visit.
///
/// The peer must already be federated: its OAuth client registration
/// (`peers.oauth_client_id`) is reused rather than re-running dynamic client
/// registration, so the delegation is a second grant to the same client.
pub async fn begin(
    state: &AppState,
    workspace_id: Uuid,
    peer_id: Uuid,
    granted_by: Option<Uuid>,
) -> Result<String, AppError> {
    let peer = PeerRepo::new(state.pool.clone())
        .get(peer_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer not found".into()))?;
    let client_id = peer.oauth_client_id.clone().ok_or_else(|| {
        AppError::BadRequest(
            "peer has no OAuth client registration; complete peer federation first".into(),
        )
    })?;

    let discovery = discover(state, &peer).await?;
    let code_verifier = crate::auth::random_url_safe_string();
    let code_challenge = crate::auth::pkce_challenge(&code_verifier);
    let nonce = crate::auth::random_url_safe_string();
    let redirect = redirect_uri(state);

    WorkspacePeerDelegationRepo::new(state.pool.clone())
        .begin_pending(
            workspace_id,
            peer_id,
            &nonce,
            &code_verifier,
            &client_id,
            &redirect,
            &discovery.token_endpoint,
            granted_by,
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(format!(
        "{endpoint}?response_type=code&client_id={client_id}&redirect_uri={redirect}&code_challenge={challenge}&code_challenge_method=S256&scope=mcp&state={state_param}",
        endpoint = discovery.authorization_endpoint,
        client_id = urlencoding::encode(&client_id),
        redirect = urlencoding::encode(&redirect),
        challenge = urlencoding::encode(&code_challenge),
        state_param = urlencoding::encode(&nonce),
    ))
}

/// Result of a completed delegation, for the callback's redirect and audit.
pub struct Delegated {
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
}

/// Complete the dance: exchange the authorization code at the peer's token
/// endpoint and store the delegated token for (workspace, peer).
///
/// The pending row is consumed atomically, so a replayed `state` finds nothing
/// and returns 400 rather than re-running the exchange.
pub async fn complete(state: &AppState, nonce: &str, code: &str) -> Result<Delegated, AppError> {
    let repo = WorkspacePeerDelegationRepo::new(state.pool.clone());
    let pending = repo
        .consume_pending(nonce)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("invalid or expired delegation state".into()))?;

    let tokens: crate::services::peer_oauth::TokenResp = state
        .http
        .post(&pending.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", pending.code_verifier.as_str()),
            ("client_id", pending.oauth_client_id.as_str()),
            ("redirect_uri", pending.redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AppError::BrokerUpstream(format!("peer token exchange failed: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::BrokerUpstream(format!("peer token exchange status: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::BrokerUpstream(format!("peer token response is not json: {e}")))?;

    let access_ciphertext = token_crypto::encrypt_versioned(tokens.access_token.as_bytes())
        .map_err(AppError::Internal)?;
    let refresh_ciphertext = tokens
        .refresh_token
        .as_deref()
        .map(|token| token_crypto::encrypt_versioned(token.as_bytes()))
        .transpose()
        .map_err(AppError::Internal)?;
    let expires_at = tokens
        .expires_in
        .map(|seconds| Utc::now() + Duration::seconds(seconds));

    let delegation = repo
        .upsert_tokens(
            pending.workspace_id,
            pending.peer_id,
            &pending.oauth_client_id,
            &pending.token_endpoint,
            &access_ciphertext,
            refresh_ciphertext.as_deref(),
            expires_at,
            pending.granted_by,
        )
        .await
        .map_err(AppError::Internal)?;

    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerGrant,
            delegation.org_id,
            delegation.granted_by,
            None,
            Some(delegation.peer_id),
            None,
            "success",
            json!({
                "scope": "workspace_peer",
                "workspaceId": delegation.workspace_id,
                "peerId": delegation.peer_id,
                "expiresAt": delegation.token_expires_at,
            }),
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(Delegated {
        workspace_id: delegation.workspace_id,
        peer_id: delegation.peer_id,
    })
}

/// The brokered delegated token for this peer handle's workspace scope, if the
/// handle carries one and a usable delegation is stored for it.
///
/// Returns `Ok(None)` — fall through to the next precedence tier — when the
/// handle is peer-global, when no delegation exists, or when the stored token is
/// expired and carries no refresh token (an unusable delegation is equivalent to
/// a deleted one; the operator re-runs the dance). A refresh that reaches the
/// peer and fails is propagated as an error rather than silently downgrading to
/// a weaker credential.
pub async fn resolve(pool: &PgPool, http: &reqwest::Client, peer: &Peer) -> Result<Option<String>> {
    let Some(workspace_id) = peer.workspace_scope else {
        return Ok(None);
    };
    let repo = WorkspacePeerDelegationRepo::new(pool.clone());
    let Some(material) = repo.material_for(workspace_id, peer.id).await? else {
        return Ok(None);
    };

    if token_is_fresh(material.token_expires_at) {
        let plaintext = token_crypto::decrypt_versioned(&material.access_token_ciphertext)
            .context("failed to decrypt delegated peer access token")?;
        return String::from_utf8(plaintext)
            .context("delegated peer access token is not utf-8")
            .map(Some);
    }

    let Some(refresh_ciphertext) = material.refresh_token_ciphertext.as_deref() else {
        tracing::warn!(
            peer_id = %peer.id,
            workspace_id = %workspace_id,
            "delegated peer token expired with no refresh token; falling back to the next \
             credential tier"
        );
        return Ok(None);
    };
    let refresh_plaintext = token_crypto::decrypt_versioned(refresh_ciphertext)
        .context("failed to decrypt delegated peer refresh token")?;
    let refresh_token = String::from_utf8(refresh_plaintext)
        .context("delegated peer refresh token is not utf-8")?;

    let tokens: crate::services::peer_oauth::TokenResp = http
        .post(&material.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", material.oauth_client_id.as_str()),
        ])
        .send()
        .await
        .context("delegated peer token refresh request failed")?
        .error_for_status()
        .context("delegated peer token refresh status")?
        .json()
        .await
        .context("delegated peer token refresh json")?;

    let access_ciphertext = token_crypto::encrypt_versioned(tokens.access_token.as_bytes())
        .context("failed to encrypt refreshed delegated peer access token")?;
    let refresh_ciphertext = tokens
        .refresh_token
        .as_deref()
        .map(|token| token_crypto::encrypt_versioned(token.as_bytes()))
        .transpose()
        .context("failed to encrypt refreshed delegated peer refresh token")?;
    let expires_at = Utc::now() + Duration::seconds(tokens.expires_in.unwrap_or(3600));
    repo.update_refreshed(
        material.id,
        &access_ciphertext,
        refresh_ciphertext.as_deref(),
        Some(expires_at),
    )
    .await?;

    Ok(Some(tokens.access_token))
}

fn token_is_fresh(expires_at: Option<DateTime<Utc>>) -> bool {
    expires_at
        .map(|at| at > Utc::now() + Duration::seconds(REFRESH_SKEW_SECONDS))
        .unwrap_or(true)
}

/// Read the peer's authorization-server metadata and reject any endpoint that
/// does not live on the peer's own host, so a compromised discovery document
/// cannot redirect the operator's consent or the code exchange elsewhere.
async fn discover(state: &AppState, peer: &Peer) -> Result<PeerDiscovery, AppError> {
    let base = peer.mcp_url.trim_end_matches('/');
    let discovery_url = format!("{base}/.well-known/oauth-authorization-server");
    let value =
        crate::services::peer_oauth::fetch_peer_metadata(state, &peer.mcp_url, &discovery_url)
            .await
            .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    let discovery: PeerDiscovery = serde_json::from_value(value)
        .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    crate::services::peer_oauth::verify_peer_endpoint_hosts(&peer.mcp_url, &discovery)?;
    Ok(discovery)
}
