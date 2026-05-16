use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    auth::{
        clear_session_set_cookie, cookie_secure_attr, extract_session_id_from_header,
        mode_from_env, session_key_from_env, AuthMode, OIDC_ISSUER_COOKIE, OIDC_STATE_COOKIE,
        OIDC_VERIFIER_COOKIE,
    },
    error::AppError,
    repos::{TrustIssuerRepo, UserSessionRepo},
    services::{IdentityAuditWriter, IdpService, SessionService},
    state::AppState,
};

#[derive(Deserialize)]
pub struct LoginQuery {
    pub issuer: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
}

/// GET /auth/login?issuer=<url>
///
/// - local mode: returns 400 (auth mode local; login not applicable)
/// - oidc mode: looks up trust_issuer; if not found 404; else 302 to authorize URL
pub async fn login(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    if mode_from_env() == AuthMode::Local {
        return Ok((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "auth mode local; login not applicable"
            })),
        )
            .into_response());
    }

    let issuer_url = q
        .issuer
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::BadRequest("missing 'issuer' query parameter".into()))?;

    let org_id = resolve_default_org_id(&state).await?;

    let ti_repo = TrustIssuerRepo::new(state.pool.clone());
    let trust_issuer = if let Ok(id) = uuid::Uuid::parse_str(&issuer_url) {
        ti_repo
            .find_by_id(org_id, id)
            .await
            .map_err(AppError::Internal)?
    } else {
        ti_repo
            .find_by_issuer_url(org_id, &issuer_url)
            .await
            .map_err(AppError::Internal)?
    }
    .ok_or_else(|| {
        AppError::BadRequest(format!(
            "no trust_issuer registered for issuer={}",
            issuer_url
        ))
    })?;

    let redirect_uri = format!("{}/auth/callback", state.config.oauth_issuer);
    let (auth_url, state_token, verifier) = IdpService::new(&state.http)
        .authorize_url(&trust_issuer, &redirect_uri)
        .await
        .map_err(AppError::Internal)?;

    // Store state + verifier in short-lived cookies on the response.
    let state_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly;{} SameSite=Lax",
        OIDC_STATE_COOKIE,
        state_token,
        cookie_secure_attr()
    );
    let verifier_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly;{} SameSite=Lax",
        OIDC_VERIFIER_COOKIE,
        verifier,
        cookie_secure_attr()
    );
    let issuer_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly;{} SameSite=Lax",
        OIDC_ISSUER_COOKIE,
        percent_encode(&trust_issuer.issuer_url),
        cookie_secure_attr()
    );

    let resp = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, auth_url)
        .header(header::SET_COOKIE, state_cookie)
        .header(header::SET_COOKIE, verifier_cookie)
        .header(header::SET_COOKIE, issuer_cookie)
        .body(axum::body::Body::empty())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to build response: {}", e)))?;

    Ok(resp)
}

/// GET /auth/callback?code=&state=
///
/// Validates that the `state` query param matches the `ione_oidc_state` cookie.
/// On mismatch returns 400.
pub async fn callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, AppError> {
    let state_param = q.state.as_deref().unwrap_or("");

    // Extract stored state from cookie header.
    let stored_state = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| extract_cookie_value(s, OIDC_STATE_COOKIE).map(String::from));

    match stored_state.as_deref() {
        None => {
            return Err(AppError::BadRequest(
                "missing or invalid oidc state cookie".into(),
            ))
        }
        Some(stored) if stored != state_param => {
            return Err(AppError::BadRequest("state mismatch".into()))
        }
        _ => {}
    }

    let code = q.code.as_deref().unwrap_or("");
    if code.is_empty() {
        return Err(AppError::BadRequest("missing 'code' parameter".into()));
    }

    let org_id = resolve_default_org_id(&state).await?;
    let verifier = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| extract_cookie_value(s, OIDC_VERIFIER_COOKIE))
        .map(str::to_owned)
        .ok_or_else(|| AppError::BadRequest("missing oidc verifier cookie".into()))?;
    let issuer_url = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| extract_cookie_value(s, OIDC_ISSUER_COOKIE))
        .map(percent_decode)
        .ok_or_else(|| AppError::BadRequest("missing oidc issuer cookie".into()))?;
    let ti = TrustIssuerRepo::new(state.pool.clone())
        .find_by_issuer_url(org_id, &issuer_url)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("unknown issuer".into()))?;
    let redirect_uri = format!("{}/auth/callback", state.config.oauth_issuer);
    let claims = IdpService::new(&state.http)
        .exchange_code_for_claims(&ti, code, &verifier, &redirect_uri, state_param)
        .await
        .map_err(|e| AppError::BadRequest(format!("oidc callback failed: {e}")))?;
    let user =
        crate::services::claim_mapper::ClaimMapper::map_to_user(&state.pool, org_id, &ti, &claims)
            .await
            .map_err(AppError::Internal)?;
    let audit = IdentityAuditWriter::new(&state.pool);
    let (_session_id, cookie) = SessionService::new(&state.pool, &audit)
        .create(user.id, user.org_id, "oidc")
        .await
        .map_err(AppError::Internal)?;

    let resp = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/")
        .header(header::SET_COOKIE, cookie)
        .header(header::SET_COOKIE, clear_oidc_cookie(OIDC_STATE_COOKIE))
        .header(header::SET_COOKIE, clear_oidc_cookie(OIDC_VERIFIER_COOKIE))
        .header(header::SET_COOKIE, clear_oidc_cookie(OIDC_ISSUER_COOKIE))
        .body(axum::body::Body::empty())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to build response: {}", e)))?;
    Ok(resp)
}

fn percent_decode(input: &str) -> String {
    urlencoding::decode(input)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| input.to_string())
}

/// POST /auth/logout — clears session cookie, returns 204.
pub async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    if let Some(session_id) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| extract_session_id_from_header(&session_key_from_env(), s))
    {
        if let Ok(Some(session)) = UserSessionRepo::new(state.pool.clone())
            .find_active(session_id)
            .await
        {
            let audit = IdentityAuditWriter::new(&state.pool);
            let service = SessionService::new(&state.pool, &audit);
            let _ = service.revoke_with_audit(&session, None).await;
        }
    }
    let clear = clear_session_set_cookie();
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, clear)]).into_response()
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn extract_cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(name) {
            if let Some(val) = rest.strip_prefix('=') {
                return Some(val);
            }
        }
    }
    None
}

fn clear_oidc_cookie(name: &str) -> String {
    format!(
        "{name}=; Max-Age=0; Path=/; HttpOnly;{} SameSite=Lax",
        cookie_secure_attr()
    )
}

async fn resolve_default_org_id(state: &AppState) -> Result<uuid::Uuid, AppError> {
    sqlx::query_scalar::<_, uuid::Uuid>("SELECT org_id FROM users WHERE id = $1 LIMIT 1")
        .bind(state.default_user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to resolve org_id: {}", e)))
}

/// Percent-encode a string for use in a URL query parameter (RFC 3986 unreserved chars pass through).
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}
