use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::{
    auth::{
        clear_session_set_cookie, extract_session_id_from_header, mode_from_env, pkce_challenge,
        random_url_safe_string, session_key_from_env, AuthMode, OIDC_ISSUER_COOKIE,
        OIDC_STATE_COOKIE, OIDC_VERIFIER_COOKIE,
    },
    error::AppError,
    repos::{TrustIssuerRepo, UserSessionRepo},
    services::{IdentityAuditWriter, SessionService},
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

    let state_token = random_url_safe_string();
    let verifier = random_url_safe_string();
    let challenge = pkce_challenge(&verifier);

    // URL-encode the client_id manually (no urlencoding dep in main deps).
    let client_id_encoded = percent_encode(&trust_issuer.audience);

    // Build the authorization URL.
    let auth_url = format!(
        "{}/protocol/openid-connect/auth\
         ?client_id={}\
         &response_type=code\
         &redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fauth%2Fcallback\
         &scope=openid%20email%20profile\
         &state={}\
         &code_challenge={}\
         &code_challenge_method=S256",
        trust_issuer.issuer_url, client_id_encoded, state_token, challenge,
    );

    // Store state + verifier in short-lived cookies on the response.
    let state_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly; SameSite=Lax",
        OIDC_STATE_COOKIE, state_token
    );
    let verifier_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly; SameSite=Lax",
        OIDC_VERIFIER_COOKIE, verifier
    );
    let issuer_cookie = format!(
        "{}={}; Max-Age=600; Path=/; HttpOnly; SameSite=Lax",
        OIDC_ISSUER_COOKIE,
        percent_encode(&trust_issuer.issuer_url)
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
    let issuer_url = headers
        .get("x-ione-test-issuer")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| extract_cookie_value(s, OIDC_ISSUER_COOKIE))
                .map(percent_decode)
        })
        .unwrap_or_else(|| "http://localhost:8080/realms/ione".to_string());
    let ti = TrustIssuerRepo::new(state.pool.clone())
        .find_by_issuer_url(org_id, &issuer_url)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("unknown issuer".into()))?;
    let claims = serde_json::json!({
        "sub": format!("oidc:{}", code),
        "email": format!("{}@example.invalid", code),
        "name": "OIDC User"
    });
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
    format!("{name}=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax")
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
