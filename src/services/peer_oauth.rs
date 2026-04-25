use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize)]
pub struct PeerDiscovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    #[serde(default)]
    pub client_id_metadata_document_supported: bool,
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
    let client = reqwest::Client::new();
    let discovery_url = format!("{peer_url}/.well-known/oauth-authorization-server");
    let disc_value = crate::util::safe_http::fetch_public_metadata(
        &discovery_url,
        64_000,
        std::time::Duration::from_secs(5),
    )
    .await
    .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    let disc: PeerDiscovery = serde_json::from_value(disc_value)
        .map_err(|_| AppError::BadRequest("invalid peer metadata".into()))?;
    verify_peer_endpoint_hosts(peer_url, &disc)?;

    let self_client_metadata_url = format!("{}/.well-known/mcp-client", state.config.oauth_issuer);
    let redirect_uri = format!("{}/api/v1/peers/callback", state.config.oauth_issuer);

    let register_resp: RegisterResp = if disc.client_id_metadata_document_supported {
        client
            .post(&disc.registration_endpoint)
            .json(&RegisterCimd {
                client_metadata_url: &self_client_metadata_url,
            })
            .send()
            .await
            .context("peer register (CIMD)")?
            .error_for_status()
            .context("peer register status")?
            .json()
            .await
            .context("peer register json")?
    } else {
        let body = serde_json::json!({
            "client_name": "IONe",
            "redirect_uris": [redirect_uri.clone()],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "scope": "mcp",
            "token_endpoint_auth_method": "none"
        });
        client
            .post(&disc.registration_endpoint)
            .json(&body)
            .send()
            .await
            .context("peer register (DCR)")?
            .error_for_status()
            .context("peer register status")?
            .json()
            .await
            .context("peer register json")?
    };

    let code_verifier = generate_opaque(32);
    let code_challenge =
        general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));

    let authorize_url = format!(
        "{endpoint}?response_type=code&client_id={client_id}&redirect_uri={redirect}&code_challenge={challenge}&code_challenge_method=S256&scope=mcp&state={peer_id}",
        endpoint = disc.authorization_endpoint,
        client_id = urlencoding::encode(&register_resp.client_id),
        redirect = urlencoding::encode(&redirect_uri),
        challenge = urlencoding::encode(&code_challenge),
        peer_id = peer_id,
    );

    let peer_repo = crate::repos::PeerRepo::new(state.pool.clone());
    peer_repo
        .begin_oauth(peer_id, &register_resp.client_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(BeginResp {
        authorize_url,
        pending: PendingFederation {
            peer_id,
            peer_url: peer_url.to_string(),
            discovery: disc,
            code_verifier,
            code_challenge,
            client_id: register_resp.client_id,
            redirect_uri,
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
    let client = reqwest::Client::new();
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", &pending.code_verifier),
        ("client_id", &pending.client_id),
        ("redirect_uri", &pending.redirect_uri),
    ];
    let tokens: TokenResp = client
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
            expires_at,
        )
        .await?;
    Ok(())
}

fn verify_peer_endpoint_hosts(peer_url: &str, disc: &PeerDiscovery) -> Result<(), AppError> {
    let peer_host = url::Url::parse(peer_url)
        .map_err(|_| AppError::BadRequest("invalid peerUrl".into()))?
        .host_str()
        .ok_or_else(|| AppError::BadRequest("invalid peerUrl".into()))?
        .to_string();

    for endpoint in [
        &disc.authorization_endpoint,
        &disc.token_endpoint,
        &disc.registration_endpoint,
    ] {
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
