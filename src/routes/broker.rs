use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Json, Redirect},
    Extension,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    repos::BrokerCredentialRepo,
    services::{IdentityAuditWriter, IdentityEvent},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginConnectionBody {
    pub provider: String,
    pub scopes: Option<Vec<String>>,
    pub label: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginConnectionResp {
    pub connection_id: Uuid,
    pub authorize_url: String,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<crate::models::BrokerCredential>>, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let rows = BrokerCredentialRepo::new(state.pool.clone())
        .list_for_user(ctx.user_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(rows))
}

pub async fn begin(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<BeginConnectionBody>,
) -> Result<Json<BeginConnectionResp>, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let provider = load_provider(&body.provider)?;
    let scopes = body.scopes.unwrap_or(provider.scopes_required);
    let label = body.label.unwrap_or_default();
    let state_token = random_token();
    let code_verifier = random_token();
    let code_challenge = crate::auth::pkce_challenge(&code_verifier);
    let row = BrokerCredentialRepo::new(state.pool.clone())
        .create_pending(
            ctx.user_id,
            ctx.org_id,
            &body.provider,
            &label,
            &scopes,
            &state_token,
            &code_verifier,
            chrono::Utc::now() + chrono::Duration::minutes(10),
        )
        .await
        .map_err(AppError::Internal)?;
    let authorize_url = format!(
        "{}?response_type=code&client_id=ione&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.authorize_url,
        urlencoding::encode(&format!("{}/auth/broker/callback", state.config.oauth_issuer)),
        urlencoding::encode(&scopes.join(" ")),
        urlencoding::encode(&state_token),
        urlencoding::encode(&code_challenge)
    );
    Ok(Json(BeginConnectionResp {
        connection_id: row.id,
        authorize_url,
    }))
}

pub async fn callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, AppError> {
    if let Some(err) = q.error {
        return Err(AppError::BadRequest(err));
    }
    let code = q
        .code
        .ok_or_else(|| AppError::BadRequest("missing code".into()))?;
    let state_token = q
        .state
        .ok_or_else(|| AppError::BadRequest("missing state".into()))?;
    let repo = BrokerCredentialRepo::new(state.pool.clone());
    let row = repo
        .find_by_state(&state_token)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("invalid broker state".into()))?;
    if row
        .state_expires_at
        .map(|ts| ts < chrono::Utc::now())
        .unwrap_or(true)
    {
        return Err(AppError::BadRequest("broker_state_expired".into()));
    }
    let provider = load_provider(&row.provider)?;
    let token_resp: serde_json::Value = reqwest::Client::new()
        .post(provider.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("code_verifier", row.code_verifier.as_deref().unwrap_or("")),
        ])
        .send()
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .error_for_status()
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .json()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let access = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| AppError::BadRequest("token response missing access_token".into()))?;
    let refresh = token_resp["refresh_token"].as_str();
    let expires_at = token_resp["expires_in"]
        .as_i64()
        .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));
    let access_cipher = crate::util::token_crypto::encrypt_versioned(access.as_bytes())
        .map_err(AppError::Internal)?;
    let refresh_cipher = refresh
        .map(|r| crate::util::token_crypto::encrypt_versioned(r.as_bytes()))
        .transpose()
        .map_err(AppError::Internal)?;
    repo.store_tokens(
        row.id,
        &access_cipher,
        refresh_cipher.as_deref(),
        expires_at,
    )
    .await
    .map_err(AppError::Internal)?;
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerGrant,
            row.org_id,
            Some(row.user_id),
            None,
            None,
            None,
            "success",
            json!({"provider": row.provider}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(Redirect::to("/connections.html"))
}

pub async fn refresh(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerRefresh,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({"id": id}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let rows = BrokerCredentialRepo::new(state.pool.clone())
        .delete(ctx.user_id, id)
        .await
        .map_err(AppError::Internal)?;
    if rows == 0 {
        return Err(AppError::NotFound("broker connection not found".into()));
    }
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerRevoke,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({"id": id}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

struct Provider {
    authorize_url: String,
    token_url: String,
    scopes_required: Vec<String>,
}

fn load_provider(name: &str) -> Result<Provider, AppError> {
    if name != "generic-test" {
        return Err(AppError::BadRequest("unknown broker provider".into()));
    }
    Ok(Provider {
        authorize_url: std::env::var("IONE_TEST_AUTHORIZE_URL")
            .unwrap_or_else(|_| "http://localhost:3901/authorize".into()),
        token_url: std::env::var("IONE_TEST_TOKEN_URL")
            .unwrap_or_else(|_| "http://localhost:3901/token".into()),
        scopes_required: Vec::new(),
    })
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
