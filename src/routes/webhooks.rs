use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    response::Json,
    Extension,
};
use chrono::{DateTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tracing::warn;
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    models::{ActorKind, PeerStatus},
    repos::{AuditEventRepo, PeerRepo},
    routes::peers::ensure_peer_in_org,
    services::webhook_ingress::{ingest_webhook_event, IngestOutcome},
    state::AppState,
    util::token_crypto::{decrypt_webhook_secret, encrypt_webhook_secret},
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionWebhookResponse {
    pub peer_id: Uuid,
    pub signing_secret: String,
    pub webhook_url: String,
}

#[derive(Deserialize)]
pub struct WebhookEnvelope {
    pub id: String,
    pub r#type: String,
    pub occurred_at: DateTime<Utc>,
    pub peer_id: Uuid,
    pub foreign_tenant_id: String,
    pub severity: Option<String>,
    pub data: Value,
    pub approval_required: bool,
}

#[derive(Serialize)]
pub struct WebhookAckResponse {
    pub ok: bool,
    pub duplicate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_ids: Option<Vec<Uuid>>,
}

pub async fn provision_webhook(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<ProvisionWebhookResponse>, AppError> {
    ensure_peer_in_org(&state, peer_id, ctx.org_id).await?;

    let peer_repo = PeerRepo::new(state.pool.clone());
    let rotated = peer_repo
        .get_with_webhook_secret(peer_id)
        .await
        .map_err(AppError::Internal)?
        .and_then(|(_, secret)| secret)
        .is_some();

    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let signing_secret = hex::encode(raw);
    let ciphertext =
        encrypt_webhook_secret(signing_secret.as_bytes()).map_err(AppError::Internal)?;
    peer_repo
        .set_webhook_secret(peer_id, &ciphertext)
        .await
        .map_err(AppError::Internal)?;

    AuditEventRepo::new(state.pool.clone())
        .insert(
            None,
            ActorKind::User,
            &ctx.user_id.to_string(),
            "webhook.provisioned",
            "peer",
            Some(peer_id),
            json!({ "peer_id": peer_id, "rotated": rotated }),
        )
        .await
        .map_err(AppError::Internal)?;

    let base = state.config.oauth_issuer.trim_end_matches('/');
    Ok(Json(ProvisionWebhookResponse {
        peer_id,
        signing_secret,
        webhook_url: format!("{base}/webhooks/peer/{peer_id}"),
    }))
}

pub async fn receive_webhook(
    State(state): State<AppState>,
    Path(peer_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WebhookAckResponse>, AppError> {
    let peer_repo = PeerRepo::new(state.pool.clone());
    let Some((peer, maybe_ciphertext)) = peer_repo
        .get_with_webhook_secret(peer_id)
        .await
        .map_err(AppError::Internal)?
    else {
        return Err(AppError::WebhookUnauthorized);
    };
    if peer.status != PeerStatus::Active {
        return Err(AppError::WebhookUnauthorized);
    }
    let Some(ciphertext) = maybe_ciphertext else {
        return Err(AppError::WebhookUnauthorized);
    };
    let signing_secret = decrypt_webhook_secret(&ciphertext).map_err(|e| {
        warn!(peer_id = %peer_id, error = %e, "failed to decrypt webhook secret");
        AppError::WebhookUnauthorized
    })?;

    let sig = parse_signature(&headers)?;
    verify_signature(signing_secret.as_bytes(), &sig, &body)?;

    let env: WebhookEnvelope = serde_json::from_slice(&body).map_err(|e| {
        warn!(peer_id = %peer_id, error = %e, "webhook body parse failed");
        AppError::WebhookRejected
    })?;
    validate_timestamps(sig.timestamp, env.occurred_at)?;
    validate_envelope(peer_id, &env)?;

    match ingest_webhook_event(&state, peer_id, &env)
        .await
        .map_err(AppError::Internal)?
    {
        IngestOutcome::Created(signal_ids) => Ok(Json(WebhookAckResponse {
            ok: true,
            duplicate: false,
            signal_ids: Some(signal_ids),
        })),
        IngestOutcome::Duplicate => Ok(Json(WebhookAckResponse {
            ok: true,
            duplicate: true,
            signal_ids: None,
        })),
        IngestOutcome::NoBinding => Err(AppError::WebhookRejected),
    }
}

struct ParsedSignature {
    timestamp_raw: String,
    timestamp: i64,
    digest: Vec<u8>,
}

fn parse_signature(headers: &HeaderMap) -> Result<ParsedSignature, AppError> {
    let value = headers
        .get("X-IONe-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::WebhookRejected)?;
    let mut timestamp_raw: Option<String> = None;
    let mut digest_raw: Option<String> = None;
    for part in value.split(',') {
        let (key, val) = part.split_once('=').ok_or(AppError::WebhookRejected)?;
        match key.trim() {
            "t" if timestamp_raw.is_none() => timestamp_raw = Some(val.trim().to_string()),
            "v1" if digest_raw.is_none() => digest_raw = Some(val.trim().to_string()),
            _ => return Err(AppError::WebhookRejected),
        }
    }
    let timestamp_raw = timestamp_raw.ok_or(AppError::WebhookRejected)?;
    let timestamp = timestamp_raw
        .parse::<i64>()
        .map_err(|_| AppError::WebhookRejected)?;
    let digest_raw = digest_raw.ok_or(AppError::WebhookRejected)?;
    if digest_raw.len() != 64 {
        return Err(AppError::WebhookRejected);
    }
    let digest = hex::decode(digest_raw).map_err(|_| AppError::WebhookRejected)?;
    if digest.len() != 32 {
        return Err(AppError::WebhookRejected);
    }
    Ok(ParsedSignature {
        timestamp_raw,
        timestamp,
        digest,
    })
}

fn verify_signature(secret: &[u8], sig: &ParsedSignature, body: &Bytes) -> Result<(), AppError> {
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| AppError::WebhookUnauthorized)?;
    mac.update(sig.timestamp_raw.as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    if expected.as_slice().ct_eq(sig.digest.as_slice()).into() {
        Ok(())
    } else {
        Err(AppError::WebhookUnauthorized)
    }
}

fn validate_timestamps(timestamp: i64, occurred_at: DateTime<Utc>) -> Result<(), AppError> {
    let Some(header_at) = Utc.timestamp_opt(timestamp, 0).single() else {
        return Err(AppError::WebhookRejected);
    };
    let now_delta = (Utc::now() - header_at).num_seconds().abs();
    let event_delta = (header_at - occurred_at).num_seconds().abs();
    if now_delta <= 300 && event_delta <= 30 {
        Ok(())
    } else {
        Err(AppError::WebhookRejected)
    }
}

fn validate_envelope(path_peer_id: Uuid, env: &WebhookEnvelope) -> Result<(), AppError> {
    if env.peer_id != path_peer_id {
        return Err(AppError::WebhookRejected);
    }
    if env.id.is_empty() || env.id.len() > 255 {
        return Err(AppError::WebhookRejected);
    }
    if env.foreign_tenant_id.is_empty() || env.foreign_tenant_id.len() > 512 {
        return Err(AppError::WebhookRejected);
    }
    if !env.data.is_object() {
        return Err(AppError::WebhookRejected);
    }
    if env.r#type.is_empty()
        || env.r#type.len() > 255
        || !env.r#type.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'/' | b'-')
        })
    {
        return Err(AppError::WebhookRejected);
    }
    Ok(())
}
