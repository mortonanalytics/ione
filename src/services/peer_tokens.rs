use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    models::Peer, repos::PeerRepo, services::peer_oauth::PeerDiscovery, util::token_crypto,
};

const REFRESH_SKEW_SECONDS: i64 = 60;

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
    let token = resolve_access_token(pool, http, peer).await?;
    let first = send_with_token(http, endpoint, body, &token).await?;
    if first.status() != StatusCode::UNAUTHORIZED || !can_refresh(peer) {
        return Ok(first);
    }

    let token = refresh_access_token(pool, http, peer).await?;
    send_with_token(http, endpoint, body, &token).await
}

async fn send_with_token(
    http: &reqwest::Client,
    endpoint: &str,
    body: &Value,
    token: &str,
) -> Result<reqwest::Response> {
    let mut request = http.post(endpoint).json(body);
    if !token.is_empty() {
        request = request.bearer_auth(token);
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
