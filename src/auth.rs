use std::sync::OnceLock;

use axum::{
    extract::State,
    http::{header, Request},
    middleware::Next,
    response::Response,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::AppError,
    models::TrustIssuer,
    repos::{MembershipRepo, RoleRepo, TrustIssuerRepo, UserRepo, UserSessionRepo},
    state::AppState,
};

const SESSION_COOKIE_NAME: &str = "ione_session";
const SESSION_DEFAULT_TTL_SECS: i64 = 86_400; // 24 h

static SESSION_KEY: OnceLock<[u8; 64]> = OnceLock::new();

/// Resolved authentication context, injected into handlers via Extension.
#[derive(Clone, Debug)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub is_oidc: bool,
    /// True when authentication was via a bearer JWT from a trusted peer issuer
    /// (MCP-to-MCP or peer-to-peer call). False for cookie sessions and local fallback.
    pub is_mcp_peer: bool,
    pub active_role_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub mfa_verified: bool,
}

/// Authentication mode derived from `IONE_AUTH_MODE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Local,
    Oidc,
}

pub fn mode_from_env() -> AuthMode {
    match std::env::var("IONE_AUTH_MODE")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "oidc" => AuthMode::Oidc,
        _ => AuthMode::Local,
    }
}

/// Return the process-lifetime session key. Initialized once via `OnceLock`.
///
/// Reads `IONE_SESSION_SECRET` (base64url, ≥64 bytes) on first call. If not set
/// or too short, generates a random key and warns. The same key is returned on
/// every subsequent call within the process, so test helpers and the server
/// middleware always agree.
pub fn session_key_from_env() -> [u8; 64] {
    *SESSION_KEY.get_or_init(init_session_key)
}

fn init_session_key() -> [u8; 64] {
    if let Ok(raw) = std::env::var("IONE_SESSION_SECRET") {
        let decoded = URL_SAFE_NO_PAD
            .decode(raw.trim())
            .expect("IONE_SESSION_SECRET must be valid base64url");
        if decoded.len() >= 64 {
            let mut key = [0u8; 64];
            key.copy_from_slice(&decoded[..64]);
            return key;
        }
        tracing::warn!("IONE_SESSION_SECRET too short (<64 bytes); generating ephemeral key");
    } else {
        tracing::warn!(
            "IONE_SESSION_SECRET not set; using ephemeral session key — sessions will not \
             survive server restart"
        );
    }
    let mut key = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

// ─── Cookie helpers ──────────────────────────────────────────────────────────

/// Cookie payload: `<session_id>:<exp_unix>`.
/// HMAC-SHA256 is computed over `<payload>` using the session key.
/// Final cookie value: `<payload>.<hex_hmac>`.
fn sign_payload(key: &[u8; 64], payload: &str) -> String {
    use sha2::digest::Mac;
    type HmacSha256 = hmac::Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

fn build_cookie_value(key: &[u8; 64], session_id: Uuid, exp: DateTime<Utc>) -> String {
    let payload = format!("{}:{}", session_id, exp.timestamp());
    let sig = sign_payload(key, &payload);
    format!("{}.{}", payload, sig)
}

/// Parse and verify a cookie value. Returns `None` if signature invalid or expired.
fn parse_cookie_value(key: &[u8; 64], value: &str) -> Option<Uuid> {
    use sha2::digest::Mac;
    type HmacSha256 = hmac::Hmac<Sha256>;

    let dot = value.rfind('.')?;
    let (payload, sig_hex) = (&value[..dot], &value[dot + 1..]);

    let expected_sig = {
        let mut mac = HmacSha256::new_from_slice(key).ok()?;
        mac.update(payload.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    };

    // Constant-time compare via iterating bytes equality
    if sig_hex.len() != expected_sig.len() {
        return None;
    }
    let ok = sig_hex
        .bytes()
        .zip(expected_sig.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0;
    if !ok {
        return None;
    }

    let (session_id_str, exp_str) = payload.split_once(':')?;

    let session_id = Uuid::parse_str(session_id_str).ok()?;
    let exp_ts: i64 = exp_str.parse().ok()?;
    let exp = DateTime::from_timestamp(exp_ts, 0)?;

    if Utc::now() > exp {
        return None;
    }

    Some(session_id)
}

/// Produce a `name=value` cookie string suitable for a `Cookie:` request header
/// or `Set-Cookie:` response header. Expiry = 24 h from now.
pub async fn issue_session_cookie(user_id: Uuid) -> anyhow::Result<String> {
    let exp = Utc::now() + chrono::Duration::seconds(SESSION_DEFAULT_TTL_SECS);
    issue_session_cookie_with_expiry(user_id, exp).await
}

/// Like `issue_session_cookie` but with an explicit expiry (for tests).
pub async fn issue_session_cookie_with_expiry(
    user_id: Uuid,
    exp_utc: DateTime<Utc>,
) -> anyhow::Result<String> {
    let key = session_key_from_env();
    let value = build_cookie_value(&key, user_id, exp_utc);
    Ok(format!("{}={}", SESSION_COOKIE_NAME, value))
}

/// Extract the session_id from a `Cookie:` header value, if valid and not expired.
pub fn extract_session_id_from_header(key: &[u8; 64], cookie_header: &str) -> Option<Uuid> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(SESSION_COOKIE_NAME) {
            if let Some(val) = rest.strip_prefix('=') {
                return parse_cookie_value(key, val);
            }
        }
    }
    None
}

/// Extract user_id from an `axum::http::HeaderMap` (reads the `Cookie:` header).
pub fn extract_session_id_from_headers(
    key: &[u8; 64],
    headers: &axum::http::HeaderMap,
) -> Option<Uuid> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    extract_session_id_from_header(key, cookie_header)
}

// ─── Middleware ───────────────────────────────────────────────────────────────

/// Auth middleware. Never returns 401 — routes that require OIDC check `is_oidc`.
/// In local mode the default user is always used.
/// In oidc mode a valid session cookie upgrades to the session user.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mode = mode_from_env();
    let key = session_key_from_env();

    let session_id_from_cookie = if mode == AuthMode::Oidc {
        req.headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| extract_session_id_from_header(&key, s))
    } else {
        None
    };
    let session = if let Some(session_id) = session_id_from_cookie {
        UserSessionRepo::new(state.pool.clone())
            .find_active(session_id)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let (resolved_user_id, resolved_org_id, is_oidc, session_id, mfa_verified) = match session {
        Some(s) => (s.user_id, s.org_id, true, Some(s.id), s.mfa_verified),
        None => (
            state.default_user_id,
            resolve_org_id(&state.pool, state.default_user_id)
                .await
                .unwrap_or(Uuid::nil()),
            false,
            None,
            false,
        ),
    };

    let active_role_id = resolve_active_role_id(&state.pool, resolved_user_id).await;

    let ctx = AuthContext {
        user_id: resolved_user_id,
        org_id: resolved_org_id,
        is_oidc,
        is_mcp_peer: false,
        active_role_id,
        session_id,
        mfa_verified,
    };

    req.extensions_mut().insert(ctx);
    next.run(req).await
}

pub async fn enforce_auth(
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, AppError> {
    let mode = req
        .extensions()
        .get::<AuthMode>()
        .cloned()
        .unwrap_or_else(mode_from_env);
    if mode == AuthMode::Oidc {
        let ok = req
            .extensions()
            .get::<AuthContext>()
            .map(|c| c.is_oidc)
            .unwrap_or(false);
        if !ok {
            return Err(AppError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}

async fn resolve_org_id(pool: &PgPool, user_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT org_id FROM users WHERE id = $1 LIMIT 1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

async fn resolve_active_role_id(pool: &PgPool, user_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT role_id FROM memberships WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

pub async fn require_admin(ctx: &AuthContext, pool: &PgPool) -> Result<(), AppError> {
    let role_id = ctx.active_role_id.ok_or(AppError::Forbidden)?;
    let coc: Option<i32> = sqlx::query_scalar("SELECT coc_level FROM roles WHERE id = $1")
        .bind(role_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    if coc.unwrap_or(0) >= 80 {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

// ─── Claim → User + Membership mapping ───────────────────────────────────────

/// Public test hook: bypass the HTTP OIDC dance, run claim→user→membership
/// mapping directly. The issuer is looked up from `trust_issuers` for the
/// default org. Returns `()` on success.
pub async fn handle_test_callback(
    pool: &PgPool,
    issuer_url: &str,
    claims: serde_json::Value,
) -> anyhow::Result<()> {
    let org_id = resolve_default_org_id(pool).await?;
    let workspace_id = resolve_default_workspace_id(pool, org_id).await?;

    let ti_repo = TrustIssuerRepo::new(pool.clone());
    let trust_issuer = ti_repo
        .find_by_issuer_url(org_id, issuer_url)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("no trust_issuer registered for issuer_url={}", issuer_url)
        })?;

    map_claims_to_user_and_membership(
        pool,
        org_id,
        workspace_id,
        &trust_issuer,
        &claims,
        issuer_url,
    )
    .await
}

async fn resolve_default_org_id(pool: &PgPool) -> anyhow::Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve default org_id: {}", e))
}

async fn resolve_default_workspace_id(pool: &PgPool, org_id: Uuid) -> anyhow::Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM workspaces WHERE org_id = $1 AND name = 'Operations' LIMIT 1",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve default workspace_id: {}", e))
}

async fn map_claims_to_user_and_membership(
    pool: &PgPool,
    org_id: Uuid,
    workspace_id: Uuid,
    trust_issuer: &TrustIssuer,
    claims: &serde_json::Value,
    issuer_url: &str,
) -> anyhow::Result<()> {
    let mapping = &trust_issuer.claim_mapping;

    let sub = claims["sub"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("claims missing 'sub'"))?;

    let email_claim = mapping["email_claim"].as_str().unwrap_or("email");
    let name_claim = mapping["name_claim"].as_str().unwrap_or("name");
    let role_claim = mapping["role_claim"].as_str().unwrap_or("ione_role");
    let coc_level_claim = mapping["coc_level_claim"]
        .as_str()
        .unwrap_or("ione_coc_level");

    let email = claims[email_claim]
        .as_str()
        .or_else(|| claims["preferred_username"].as_str())
        .unwrap_or(sub);

    let display_name = claims[name_claim].as_str().unwrap_or(email);

    let role_name = claims[role_claim].as_str().unwrap_or("member");
    let coc_level = claims[coc_level_claim].as_i64().unwrap_or(0) as i32;

    let workspace_name = mapping["workspace_name"].as_str().unwrap_or("Operations");

    let target_workspace_id = if workspace_name == "Operations" {
        workspace_id
    } else {
        resolve_workspace_by_name(pool, org_id, workspace_name)
            .await?
            .unwrap_or(workspace_id)
    };

    let user_repo = UserRepo::new(pool.clone());
    let user = user_repo
        .upsert_by_oidc_subject(org_id, email, display_name, sub)
        .await?;

    let role_repo = RoleRepo::new(pool.clone());
    let role = role_repo
        .upsert(target_workspace_id, role_name, coc_level)
        .await?;

    let federated_claim_ref = format!("{}@{}", sub, issuer_url);

    let membership_repo = MembershipRepo::new(pool.clone());
    membership_repo
        .upsert_federated(user.id, target_workspace_id, role.id, &federated_claim_ref)
        .await?;

    Ok(())
}

async fn resolve_workspace_by_name(
    pool: &PgPool,
    org_id: Uuid,
    name: &str,
) -> anyhow::Result<Option<Uuid>> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM workspaces WHERE org_id = $1 AND name = $2 LIMIT 1",
    )
    .bind(org_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve workspace by name: {}", e))
}

// ─── Real OIDC callback (used in production, not in tests) ───────────────────

/// Placeholder OIDC state cookie name used during login/callback.
pub const OIDC_STATE_COOKIE: &str = "ione_oidc_state";
pub const OIDC_VERIFIER_COOKIE: &str = "ione_oidc_verifier";
pub const OIDC_ISSUER_COOKIE: &str = "ione_oidc_issuer";

/// Build the PKCE challenge from a verifier (base64url(sha256(verifier))).
pub fn pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a cryptographically random URL-safe base64 string (32 bytes → 43 chars).
pub fn random_url_safe_string() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

// ─── Logout helper ────────────────────────────────────────────────────────────

/// Produce a `Set-Cookie` header value that expires the session cookie.
pub fn clear_session_set_cookie() -> String {
    format!(
        "{}=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax",
        SESSION_COOKIE_NAME
    )
}

/// Produce a `Set-Cookie` header value that sets a new session cookie.
pub fn set_session_cookie_header(user_id: Uuid) -> String {
    let key = session_key_from_env();
    let exp = Utc::now() + chrono::Duration::seconds(SESSION_DEFAULT_TTL_SECS);
    let value = build_cookie_value(&key, user_id, exp);
    format!(
        "{}={}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
        SESSION_COOKIE_NAME, value, SESSION_DEFAULT_TTL_SECS
    )
}

pub fn set_session_cookie_header_for_session(session_id: Uuid, exp: DateTime<Utc>) -> String {
    let key = session_key_from_env();
    let max_age = (exp - Utc::now()).num_seconds().max(0);
    let value = build_cookie_value(&key, session_id, exp);
    format!(
        "{}={}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
        SESSION_COOKIE_NAME, value, max_age
    )
}
