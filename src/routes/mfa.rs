use axum::{extract::State, http::StatusCode, response::Json, Extension};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use data_encoding::BASE32_NOPAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    auth::AuthContext,
    error::AppError,
    repos::UserSessionRepo,
    services::{IdentityAuditWriter, IdentityEvent},
    state::AppState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaStatus {
    pub totp_enrolled: bool,
    pub recovery_codes_remaining: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResp {
    pub otpauth_uri: String,
    pub secret_b32: String,
    pub qr_svg: String,
}

#[derive(Deserialize)]
pub struct CodeBody {
    pub code: String,
}

pub async fn status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<MfaStatus>, AppError> {
    Ok(Json(load_status(&state, &ctx).await?))
}

pub async fn enroll_totp(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<EnrollResp>, AppError> {
    let mut secret = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut secret);
    let secret_b32 = BASE32_NOPAD.encode(&secret);
    let ciphertext =
        crate::util::token_crypto::encrypt_versioned(&secret).map_err(AppError::Internal)?;
    sqlx::query(
        "INSERT INTO mfa_enrollments (user_id, org_id, totp_secret_ciphertext)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE
         SET totp_secret_ciphertext = EXCLUDED.totp_secret_ciphertext, activated_at = NULL",
    )
    .bind(ctx.user_id)
    .bind(ctx.org_id)
    .bind(ciphertext)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let label = format!("IONe:{}", ctx.user_id);
    let otpauth_uri = format!(
        "otpauth://totp/{}?secret={}&issuer=IONe&algorithm=SHA1&digits=6&period=30",
        urlencoding::encode(&label),
        secret_b32
    );
    let qr_svg = qrcode::QrCode::new(otpauth_uri.as_bytes())
        .map(|qr| {
            qr.render::<char>()
                .quiet_zone(false)
                .module_dimensions(1, 1)
                .build()
        })
        .unwrap_or_default();
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaEnroll,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(EnrollResp {
        otpauth_uri,
        secret_b32,
        qr_svg,
    }))
}

pub async fn confirm_totp(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode, AppError> {
    if !verify_totp(&state, ctx.user_id, &body.code).await? {
        audit_mfa_fail(&state, &ctx).await?;
        return Err(AppError::Forbidden);
    }
    sqlx::query("UPDATE mfa_enrollments SET activated_at = now() WHERE user_id = $1")
        .bind(ctx.user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    if let Some(session_id) = ctx.session_id {
        UserSessionRepo::new(state.pool.clone())
            .mark_mfa_verified(session_id)
            .await
            .map_err(AppError::Internal)?;
    }
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaVerify,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn challenge(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode, AppError> {
    let ok = verify_totp(&state, ctx.user_id, &body.code).await?
        || consume_recovery(&state, &ctx, &body.code).await?;
    if !ok {
        audit_mfa_fail(&state, &ctx).await?;
        return Err(AppError::Forbidden);
    }
    if let Some(session_id) = ctx.session_id {
        UserSessionRepo::new(state.pool.clone())
            .mark_mfa_verified(session_id)
            .await
            .map_err(AppError::Internal)?;
    }
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaVerify,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn recovery_codes(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<String>>, AppError> {
    let viewed: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT recovery_codes_viewed_at FROM mfa_enrollments WHERE user_id = $1",
    )
    .bind(ctx.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .flatten();
    if viewed.is_some() {
        return Err(AppError::BadRequest("recovery codes already viewed".into()));
    }
    let mut codes = Vec::new();
    for _ in 0..8 {
        let mut raw = [0u8; 10];
        rand::thread_rng().fill_bytes(&mut raw);
        let code = URL_SAFE_NO_PAD.encode(raw);
        let hash = hash_code(&code);
        sqlx::query(
            "INSERT INTO mfa_recovery_codes (user_id, org_id, code_hash) VALUES ($1, $2, $3)",
        )
        .bind(ctx.user_id)
        .bind(ctx.org_id)
        .bind(hash)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
        codes.push(code);
    }
    sqlx::query("UPDATE mfa_enrollments SET recovery_codes_viewed_at = now() WHERE user_id = $1")
        .bind(ctx.user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaRecoveryViewed,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(codes))
}

pub async fn delete_totp(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<CodeBody>,
) -> Result<StatusCode, AppError> {
    if !verify_totp(&state, ctx.user_id, &body.code).await? {
        return Err(AppError::BadRequest("current TOTP code is required".into()));
    }
    sqlx::query("DELETE FROM mfa_enrollments WHERE user_id = $1")
        .bind(ctx.user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaDisable,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn load_status(state: &AppState, ctx: &AuthContext) -> Result<MfaStatus, AppError> {
    let enrolled = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM mfa_enrollments WHERE user_id = $1 AND activated_at IS NOT NULL)",
    )
    .bind(ctx.user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let remaining = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM mfa_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(ctx.user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(MfaStatus {
        totp_enrolled: enrolled,
        recovery_codes_remaining: remaining,
    })
}

async fn verify_totp(state: &AppState, user_id: uuid::Uuid, code: &str) -> Result<bool, AppError> {
    let ciphertext: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT totp_secret_ciphertext FROM mfa_enrollments WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    let Some(ciphertext) = ciphertext else {
        return Ok(false);
    };
    let secret =
        crate::util::token_crypto::decrypt_versioned(&ciphertext).map_err(AppError::Internal)?;
    let now = chrono::Utc::now().timestamp() / 30;
    for step in [now - 1, now, now + 1] {
        if totp_code(&secret, step as u64) == code {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn consume_recovery(
    state: &AppState,
    ctx: &AuthContext,
    code: &str,
) -> Result<bool, AppError> {
    let hash = hash_code(code);
    let row: Option<uuid::Uuid> = sqlx::query_scalar(
        "UPDATE mfa_recovery_codes SET used_at = now()
         WHERE id = (
             SELECT id FROM mfa_recovery_codes
             WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL
             LIMIT 1
         )
         RETURNING id",
    )
    .bind(ctx.user_id)
    .bind(hash)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    if row.is_some() {
        IdentityAuditWriter::new(&state.pool)
            .write(
                IdentityEvent::MfaRecoveryConsume,
                ctx.org_id,
                Some(ctx.user_id),
                ctx.session_id,
                None,
                None,
                "success",
                json!({}),
            )
            .await
            .map_err(AppError::Internal)?;
    }
    Ok(row.is_some())
}

async fn audit_mfa_fail(state: &AppState, ctx: &AuthContext) -> Result<(), AppError> {
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::MfaFail,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "failure",
            json!({}),
        )
        .await
        .map_err(AppError::Internal)
}

fn hash_code(code: &str) -> String {
    hex::encode(Sha256::digest(code.as_bytes()))
}

fn totp_code(secret: &[u8], counter: u64) -> String {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut msg = [0u8; 8];
    msg.copy_from_slice(&counter.to_be_bytes());
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac key");
    mac.update(&msg);
    let hash = mac.finalize().into_bytes();
    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let binary = (((hash[offset] & 0x7f) as u32) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | (hash[offset + 3] as u32);
    format!("{:06}", binary % 1_000_000)
}
