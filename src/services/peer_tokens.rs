use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    models::Peer, repos::PeerRepo, services::peer_oauth::PeerDiscovery, util::token_crypto,
};

const REFRESH_SKEW_SECONDS: i64 = 60;
static PEER_GOVERNORS: Lazy<
    DashMap<uuid::Uuid, Arc<crate::services::peer_governor::PeerGovernor>>,
> = Lazy::new(DashMap::new);

#[derive(Debug, Deserialize)]
struct RefreshTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

pub async fn resolve_access_token(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<String> {
    if peer.access_token_ciphertext.is_none() {
        return static_bearer();
    }
    if token_is_fresh(peer) {
        return decrypt_access_token(peer);
    }
    refresh_access_token(pool, http, peer).await
}

/// Like `resolve_access_token` but serializes concurrent refresh for a single peer
/// using a per-peer mutex stored in `AppState`. Prevents the token-overwrite race
/// when multiple requests for the same peer race to refresh simultaneously.
pub async fn resolve_access_token_locked(
    state: &crate::state::AppState,
    peer: &Peer,
) -> Result<String> {
    if peer.access_token_ciphertext.is_none() {
        return static_bearer();
    }
    // Fast path: token is fresh — no need to take the lock.
    if token_is_fresh(peer) {
        return decrypt_access_token(peer);
    }
    // Acquire per-peer lock before refresh to serialize concurrent callers.
    let lock = state
        .peer_refresh_locks
        .entry(peer.id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = lock.lock().await;
    // Re-check freshness under the lock: the thread that won the lock may have
    // already refreshed the token, so reload from DB before deciding to refresh.
    let fresh_peer = PeerRepo::new(state.pool.clone())
        .get(peer.id)
        .await
        .ok()
        .flatten();
    if let Some(ref reloaded) = fresh_peer {
        if token_is_fresh(reloaded) {
            return decrypt_access_token(reloaded);
        }
        return refresh_access_token(&state.pool, &state.http, reloaded).await;
    }
    refresh_access_token(&state.pool, &state.http, peer).await
}

pub async fn refresh_access_token(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
) -> Result<String> {
    let refresh_ciphertext = peer
        .refresh_token_ciphertext
        .as_deref()
        .context("peer has no refresh token ciphertext; re-authorization required")?;
    let refresh_token = token_crypto::decrypt_token(refresh_ciphertext)
        .context("failed to decrypt peer refresh token")?;
    let client_id = peer
        .oauth_client_id
        .as_deref()
        .context("peer has no oauth client id")?;
    let discovery = discover_peer(peer, http).await?;

    let tokens: RefreshTokenResp = http
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id),
        ])
        .send()
        .await
        .context("peer refresh token request failed")?
        .error_for_status()
        .context("peer refresh token status")?
        .json()
        .await
        .context("peer refresh token json")?;

    let access_hash = sha256_hex(&tokens.access_token);
    let access_ciphertext = token_crypto::encrypt_token(&tokens.access_token)
        .context("failed to encrypt refreshed peer access token")?;
    let refresh_hash = tokens.refresh_token.as_deref().map(sha256_hex);
    let refresh_ciphertext = tokens
        .refresh_token
        .as_deref()
        .map(token_crypto::encrypt_token)
        .transpose()
        .context("failed to encrypt refreshed peer refresh token")?;
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in.unwrap_or(3600));

    PeerRepo::new(pool.clone())
        .update_refreshed_tokens(
            peer.id,
            &access_hash,
            refresh_hash.as_deref(),
            &access_ciphertext,
            refresh_ciphertext.as_deref(),
            expires_at,
        )
        .await?;

    Ok(tokens.access_token)
}

pub async fn send_mcp_request(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
) -> Result<reqwest::Response> {
    send_mcp_request_with_session(pool, http, peer, endpoint, body, None).await
}

pub async fn send_mcp_request_with_session(
    pool: &PgPool,
    http: &reqwest::Client,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let governor = governor_for(peer.id);
    governor.acquire().await?;
    let token = resolve_access_token(pool, http, peer).await?;
    let first = match send_with_token(http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(pool, peer, &governor, response.status()).await;
            response
        }
        Err(e) => {
            record_peer_failure(pool, peer, &governor, &e).await;
            return Err(e);
        }
    };
    if first.status() != StatusCode::UNAUTHORIZED || !can_refresh(peer) {
        return Ok(first);
    }

    governor.acquire().await?;
    let token = refresh_access_token(pool, http, peer).await?;
    match send_with_token(http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(pool, peer, &governor, response.status()).await;
            Ok(response)
        }
        Err(e) => {
            record_peer_failure(pool, peer, &governor, &e).await;
            Err(e)
        }
    }
}

/// Variant of `send_mcp_request_with_session` that uses the per-peer refresh mutex
/// in `AppState` to prevent concurrent token overwrites on the same peer.
pub async fn send_mcp_request_with_state(
    state: &crate::state::AppState,
    peer: &Peer,
    endpoint: &str,
    body: &Value,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let governor = governor_for(peer.id);
    governor.acquire().await?;
    let token = resolve_access_token_locked(state, peer).await?;
    let first = match send_with_token(&state.http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(&state.pool, peer, &governor, response.status()).await;
            response
        }
        Err(e) => {
            record_peer_failure(&state.pool, peer, &governor, &e).await;
            return Err(e);
        }
    };
    if first.status() != StatusCode::UNAUTHORIZED || !can_refresh(peer) {
        return Ok(first);
    }

    governor.acquire().await?;
    let token = refresh_access_token(&state.pool, &state.http, peer).await?;
    match send_with_token(&state.http, endpoint, body, &token, mcp_session_id).await {
        Ok(response) => {
            record_peer_response(&state.pool, peer, &governor, response.status()).await;
            Ok(response)
        }
        Err(e) => {
            record_peer_failure(&state.pool, peer, &governor, &e).await;
            Err(e)
        }
    }
}

pub fn governor_snapshot(
    peer_id: uuid::Uuid,
) -> Option<crate::services::peer_governor::PeerGovernorSnapshot> {
    PEER_GOVERNORS
        .get(&peer_id)
        .map(|entry| entry.value().snapshot())
}

async fn send_with_token(
    http: &reqwest::Client,
    endpoint: &str,
    body: &Value,
    token: &str,
    mcp_session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let mut request = http.post(endpoint).json(body);
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    if let Some(session_id) = mcp_session_id {
        request = request.header("MCP-Session-Id", session_id);
    }
    request.send().await.context("HTTP send failed")
}

fn token_is_fresh(peer: &Peer) -> bool {
    peer.token_expires_at
        .map(|expires_at| {
            expires_at > chrono::Utc::now() + chrono::Duration::seconds(REFRESH_SKEW_SECONDS)
        })
        .unwrap_or(true)
}

fn can_refresh(peer: &Peer) -> bool {
    peer.refresh_token_ciphertext.is_some() && peer.oauth_client_id.is_some()
}

/// Throttle inbound peer notifications using the peer's governor. Returns false
/// when the peer has exceeded `max_per_minute` notifications in the trailing
/// minute, so callers can drop the flood instead of landing it in `stream_events`.
pub fn protocol_notification_allowed(peer_id: uuid::Uuid, max_per_minute: usize) -> bool {
    governor_for(peer_id).allow_protocol_notification(max_per_minute)
}

fn governor_for(peer_id: uuid::Uuid) -> Arc<crate::services::peer_governor::PeerGovernor> {
    PEER_GOVERNORS
        .entry(peer_id)
        .or_insert_with(|| {
            let rps = std::env::var("IONE_PEER_CALL_RPS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(10);
            let burst = std::env::var("IONE_PEER_CALL_BURST")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(20);
            Arc::new(crate::services::peer_governor::PeerGovernor::new(
                rps, burst,
            ))
        })
        .clone()
}

async fn record_peer_response(
    pool: &PgPool,
    peer: &Peer,
    governor: &crate::services::peer_governor::PeerGovernor,
    status: StatusCode,
) {
    if status.is_server_error() {
        if governor.record_peer_failure() {
            let _ = PeerRepo::new(pool.clone())
                .set_session_status(peer.id, "error", Some("peer circuit breaker opened"))
                .await;
        }
    } else if !status.is_client_error() {
        governor.record_success();
    }
}

async fn record_peer_failure(
    pool: &PgPool,
    peer: &Peer,
    governor: &crate::services::peer_governor::PeerGovernor,
    error: &anyhow::Error,
) {
    if governor.record_peer_failure() {
        let _ = PeerRepo::new(pool.clone())
            .set_session_status(peer.id, "error", Some(&error.to_string()))
            .await;
    }
}

fn decrypt_access_token(peer: &Peer) -> Result<String> {
    let ciphertext = peer
        .access_token_ciphertext
        .as_deref()
        .context("peer access token is unavailable")?;
    token_crypto::decrypt_token(ciphertext).context("failed to decrypt peer access token")
}

fn static_bearer() -> Result<String> {
    std::env::var("IONE_OAUTH_STATIC_BEARER")
        .context("peer has no token and IONE_OAUTH_STATIC_BEARER is not set")
}

async fn discover_peer(peer: &Peer, http: &reqwest::Client) -> Result<PeerDiscovery> {
    let discovery_url = format!(
        "{}/.well-known/oauth-authorization-server",
        peer.mcp_url.trim_end_matches('/')
    );
    let discovery: PeerDiscovery = http
        .get(&discovery_url)
        .send()
        .await
        .context("peer discovery request failed")?
        .error_for_status()
        .context("peer discovery status")?
        .json()
        .await
        .context("peer discovery json")?;
    verify_refresh_endpoint_host(peer, &discovery)?;
    Ok(discovery)
}

fn verify_refresh_endpoint_host(peer: &Peer, discovery: &PeerDiscovery) -> Result<()> {
    let peer_url = url::Url::parse(&peer.mcp_url).context("invalid peer mcp_url")?;
    let peer_host = peer_url
        .host_str()
        .context("peer mcp_url missing host")?
        .to_string();
    let token_url =
        url::Url::parse(&discovery.token_endpoint).context("invalid peer token endpoint")?;
    let token_host = token_url
        .host_str()
        .context("peer token endpoint missing host")?
        .to_string();
    anyhow::ensure!(token_host == peer_host, "peer token endpoint host mismatch");
    anyhow::ensure!(
        token_url.scheme() == peer_url.scheme(),
        "peer token endpoint scheme mismatch"
    );
    Ok(())
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}
