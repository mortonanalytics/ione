use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize)]
pub struct PeerDiscovery {
    /// RFC 8414 `issuer`. Validated against the peer host like every other URL in
    /// the document (see [`verify_peer_endpoint_hosts`]); a document claiming to
    /// be issued by somewhere else is not this peer's document.
    #[serde(default)]
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// RFC 7591 dynamic client registration. **Optional**: a peer that advertises
    /// `client_id_metadata_document_supported` needs no registration step at all,
    /// because IONe can present its own published client-metadata document URL as
    /// the `client_id`. Contract §2's endpoint table does not list this endpoint,
    /// so requiring it kept conforming peers out.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// RFC 7009 revocation endpoint. IONe does not call it; it is host-validated
    /// with the rest so a tampered document cannot point a future revoke
    /// elsewhere.
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    #[serde(default)]
    pub client_id_metadata_document_supported: bool,
}

/// RFC 8414 §3: authorization-server metadata is published at the **origin**,
/// `https://host[:port]/.well-known/oauth-authorization-server`, not below the
/// resource path. `peers.mcp_url` carries the MCP path (`https://host/mcp`), so
/// the discovery URL is derived from its origin.
fn origin_discovery_url(peer_url: &str) -> Result<String, AppError> {
    let mut url =
        url::Url::parse(peer_url).map_err(|_| AppError::BadRequest("invalid peerUrl".into()))?;
    if url.host_str().is_none() {
        return Err(AppError::BadRequest("invalid peerUrl".into()));
    }
    url.set_path("/.well-known/oauth-authorization-server");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

/// Fetch and validate a peer's authorization-server metadata.
///
/// Tries the RFC 8414 origin location first and falls back to the pre-2026-07-25
/// `{mcp_url}/.well-known/…` location. The fallback is kept rather than dropped
/// because peers were built against the old behaviour — `tests/support/stub_peer.rs`
/// and the fixtures in `tests/identity_broker_integration.rs` and
/// `tests/credential_presentation_integration.rs` among them — and an outright
/// switch would break every one of them at the first step of the join. It logs at
/// `warn!` so the legacy location is visible and can be retired later.
///
/// Returns the document together with the URL it was actually read from, so an
/// error message can name the document the operator has to change.
pub async fn fetch_peer_discovery(
    state: &AppState,
    peer_url: &str,
) -> Result<(PeerDiscovery, String), AppError> {
    let origin_url = origin_discovery_url(peer_url)?;
    let legacy_url = format!(
        "{}/.well-known/oauth-authorization-server",
        peer_url.trim_end_matches('/')
    );

    let (value, source_url) = match fetch_peer_metadata(state, peer_url, &origin_url).await {
        Ok(value) => (value, origin_url),
        Err(origin_error) if legacy_url != origin_url => {
            let value = fetch_peer_metadata(state, peer_url, &legacy_url)
                .await
                .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
            tracing::warn!(
                peer_url,
                legacy_url,
                %origin_error,
                "peer serves OAuth discovery under its MCP path; RFC 8414 places it at the origin"
            );
            (value, legacy_url)
        }
        Err(_) => return Err(AppError::BadRequest("invalid peer metadata".into())),
    };

    let discovery: PeerDiscovery = serde_json::from_value(value)
        .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    verify_peer_endpoint_hosts(peer_url, &discovery)?;
    Ok((discovery, source_url))
}

#[derive(Debug)]
pub struct PendingFederation {
    pub peer_id: uuid::Uuid,
    pub peer_url: String,
    pub discovery: PeerDiscovery,
    pub code_verifier: String,
    pub code_challenge: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub nonce: String,
}

#[derive(Debug)]
pub struct BeginResp {
    pub authorize_url: String,
    pub pending: PendingFederation,
}

#[derive(Debug, Serialize)]
struct RegisterCimd<'a> {
    client_metadata_url: &'a str,
}

#[derive(Debug, Deserialize)]
struct RegisterResp {
    #[serde(alias = "clientId")]
    client_id: String,
}

pub async fn begin_federation(
    state: &AppState,
    peer_id: uuid::Uuid,
    peer_url: &str,
) -> Result<BeginResp, AppError> {
    let (disc, discovery_url) = fetch_peer_discovery(state, peer_url).await?;

    let self_client_metadata_url = format!("{}/.well-known/mcp-client", state.config.oauth_issuer);
    let redirect_uri = format!("{}/api/v1/peers/callback", state.config.oauth_issuer);

    // Precedence: register when the peer offers a registration endpoint (in CIMD
    // form if it supports it, otherwise RFC 7591), and fall back to presenting
    // IONe's own client-metadata document as the `client_id` only when there is
    // no registration endpoint to call. CIMD is the fallback rather than the
    // preference because a peer that publishes both — IONe's own authorization
    // server does, `routes/oauth.rs:26,33` — resolves `client_id` against its
    // registered-client table (`routes/oauth.rs:122`) and would reject a bare
    // metadata URL.
    let client_id = match disc.registration_endpoint.as_deref() {
        Some(registration_endpoint) => {
            let body = if disc.client_id_metadata_document_supported {
                serde_json::to_value(RegisterCimd {
                    client_metadata_url: &self_client_metadata_url,
                })
                .map_err(|e| AppError::Internal(e.into()))?
            } else {
                serde_json::json!({
                    "client_name": "IONe",
                    "redirect_uris": [redirect_uri.clone()],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "scope": "mcp",
                    "token_endpoint_auth_method": "none"
                })
            };
            let register_resp: RegisterResp = state
                .http
                .post(registration_endpoint)
                .json(&body)
                .send()
                .await
                .context("peer register")?
                .error_for_status()
                .context("peer register status")?
                .json()
                .await
                .context("peer register json")?;
            register_resp.client_id
        }
        None if disc.client_id_metadata_document_supported => self_client_metadata_url.clone(),
        None => {
            return Err(AppError::BadRequest(format!(
                "peer discovery document at {discovery_url} publishes no \
                 'registration_endpoint' and does not advertise \
                 'client_id_metadata_document_supported', so IONe cannot obtain a client_id. \
                 The peer must publish 'registration_endpoint' (RFC 7591 dynamic client \
                 registration) or set 'client_id_metadata_document_supported': true and accept \
                 {self_client_metadata_url} as the client_id."
            )))
        }
    };

    let code_verifier = generate_opaque(32);
    let code_challenge =
        general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let nonce = generate_opaque(32);

    let authorize_url = format!(
        "{endpoint}?response_type=code&client_id={client_id}&redirect_uri={redirect}&code_challenge={challenge}&code_challenge_method=S256&scope=mcp&state={state}",
        endpoint = disc.authorization_endpoint,
        client_id = urlencoding::encode(&client_id),
        redirect = urlencoding::encode(&redirect_uri),
        challenge = urlencoding::encode(&code_challenge),
        state = urlencoding::encode(&nonce),
    );

    let peer_repo = crate::repos::PeerRepo::new(state.pool.clone());
    peer_repo
        .begin_oauth(peer_id, &client_id)
        .await
        .map_err(AppError::Internal)?;
    sqlx::query(
        "INSERT INTO peer_oauth_pending (peer_id, nonce, code_verifier, expires_at)
         VALUES ($1, $2, $3, now() + interval '10 minutes')",
    )
    .bind(peer_id)
    .bind(&nonce)
    .bind(&code_verifier)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(BeginResp {
        authorize_url,
        pending: PendingFederation {
            peer_id,
            peer_url: peer_url.to_string(),
            discovery: disc,
            code_verifier,
            code_challenge,
            client_id,
            redirect_uri,
            nonce,
        },
    })
}

#[derive(Debug, Deserialize)]
pub struct TokenResp {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn complete_callback(
    state: &AppState,
    pending: &PendingFederation,
    code: &str,
) -> Result<()> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", &pending.code_verifier),
        ("client_id", &pending.client_id),
        ("redirect_uri", &pending.redirect_uri),
    ];
    let tokens: TokenResp = state
        .http
        .post(&pending.discovery.token_endpoint)
        .form(&form)
        .send()
        .await
        .context("peer token exchange")?
        .error_for_status()
        .context("peer token status")?
        .json()
        .await
        .context("peer token json")?;
    let access_hash = sha256_hex(&tokens.access_token);
    let access_ciphertext = crate::util::token_crypto::encrypt_token(&tokens.access_token)?;
    let refresh_ciphertext = tokens
        .refresh_token
        .as_deref()
        .map(crate::util::token_crypto::encrypt_token)
        .transpose()?;
    let refresh_hash = tokens
        .refresh_token
        .as_ref()
        .map(|t| sha256_hex(t))
        .unwrap_or_default();
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(tokens.expires_in.unwrap_or(3600));
    let peer_repo = crate::repos::PeerRepo::new(state.pool.clone());
    peer_repo
        .set_tokens(
            pending.peer_id,
            &access_hash,
            &refresh_hash,
            &access_ciphertext,
            refresh_ciphertext.as_deref(),
            expires_at,
        )
        .await?;
    Ok(())
}

pub fn verify_peer_endpoint_hosts(peer_url: &str, disc: &PeerDiscovery) -> Result<(), AppError> {
    let peer_host = url::Url::parse(peer_url)
        .map_err(|_| AppError::BadRequest("invalid peerUrl".into()))?
        .host_str()
        .ok_or_else(|| AppError::BadRequest("invalid peerUrl".into()))?
        .to_string();

    let endpoints = [
        Some(disc.authorization_endpoint.as_str()),
        Some(disc.token_endpoint.as_str()),
        disc.registration_endpoint.as_deref(),
        disc.revocation_endpoint.as_deref(),
        disc.issuer.as_deref(),
    ];
    for endpoint in endpoints.into_iter().flatten() {
        let endpoint_host = url::Url::parse(endpoint)
            .map_err(|_| AppError::BadRequest("invalid peer endpoint".into()))?
            .host_str()
            .ok_or_else(|| AppError::BadRequest("invalid peer endpoint".into()))?
            .to_string();
        if endpoint_host != peer_host {
            return Err(AppError::BadRequest(
                "peer endpoints must match peer host".into(),
            ));
        }
    }
    Ok(())
}

pub async fn fetch_peer_metadata(
    state: &AppState,
    peer_url: &str,
    metadata_url: &str,
) -> Result<serde_json::Value> {
    match crate::util::safe_http::fetch_public_metadata(
        metadata_url,
        64_000,
        std::time::Duration::from_secs(5),
    )
    .await
    {
        Ok(value) => return Ok(value),
        Err(public_error) => {
            crate::services::peer::validate_mcp_url(peer_url)
                .await
                .with_context(|| format!("public metadata fetch failed: {public_error}"))?;
        }
    }

    let resp = state
        .http
        .get(metadata_url)
        .send()
        .await
        .context("peer metadata request failed")?
        .error_for_status()
        .context("peer metadata status")?;
    resp.json().await.context("peer metadata json")
}

fn generate_opaque(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    general_purpose::URL_SAFE_NO_PAD.encode(buf)
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
