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
        .consume_by_state(&state_token)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("invalid or expired broker state".into()))?;
    let provider = load_provider(&row.provider)?;
    let token_resp: serde_json::Value = state
        .http
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

/// POST /api/v1/broker/connections/:id/refresh
///
/// Exchanges the stored refresh token at the provider's token endpoint and
/// rewrites the stored ciphertext. The audit row records the true outcome: a
/// provider failure writes `outcome = "failure"` and leaves the stored token
/// exactly as it was, so a failed refresh never costs the operator a working
/// connection.
pub async fn refresh(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let repo = BrokerCredentialRepo::new(state.pool.clone());
    let row = repo
        .find_for_user(ctx.user_id, id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("broker connection not found".into()))?;
    let provider = load_provider(&row.provider)?;

    match exchange_refresh_token(&state, &repo, &row, &provider).await {
        Ok(expires_at) => {
            audit_refresh(&state, &ctx, &row, "success", json!({"id": id})).await?;
            Ok(Json(json!({ "expiresAt": expires_at })))
        }
        Err(error) => {
            let reason = error.reason();
            audit_refresh(
                &state,
                &ctx,
                &row,
                "failure",
                json!({"id": id, "failureReason": reason}),
            )
            .await?;
            Err(error.into())
        }
    }
}

/// Why a refresh did not happen. Separated from `AppError` so the audit row and
/// the HTTP status are decided from one value.
enum RefreshFailure {
    NoRefreshToken,
    Upstream(String),
    Internal(anyhow::Error),
}

impl RefreshFailure {
    fn reason(&self) -> String {
        match self {
            Self::NoRefreshToken => "no_refresh_token".into(),
            Self::Upstream(message) => format!("upstream: {message}"),
            Self::Internal(error) => format!("internal: {error}"),
        }
    }
}

impl From<RefreshFailure> for AppError {
    fn from(failure: RefreshFailure) -> Self {
        match failure {
            RefreshFailure::NoRefreshToken => AppError::BadRequest(
                "connection has no refresh token; reconnect the provider".into(),
            ),
            RefreshFailure::Upstream(message) => AppError::BrokerUpstream(message),
            RefreshFailure::Internal(error) => AppError::Internal(error),
        }
    }
}

/// The refresh-grant exchange itself. Writes the new ciphertext only on success.
async fn exchange_refresh_token(
    state: &AppState,
    repo: &BrokerCredentialRepo,
    row: &crate::models::BrokerCredential,
    provider: &Provider,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, RefreshFailure> {
    let refresh_ciphertext = row
        .refresh_token_ciphertext
        .as_deref()
        .ok_or(RefreshFailure::NoRefreshToken)?;
    let refresh_token = decrypt_to_string(refresh_ciphertext).map_err(RefreshFailure::Internal)?;

    let token_resp: serde_json::Value = state
        .http
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", "ione"),
        ])
        .send()
        .await
        .map_err(|e| RefreshFailure::Upstream(format!("token request failed: {e}")))?
        .error_for_status()
        .map_err(|e| RefreshFailure::Upstream(format!("token endpoint returned {e}")))?
        .json()
        .await
        .map_err(|e| RefreshFailure::Upstream(format!("token response is not json: {e}")))?;

    let access = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| RefreshFailure::Upstream("token response missing access_token".into()))?;
    let refresh = token_resp["refresh_token"].as_str();
    let expires_at = token_resp["expires_in"]
        .as_i64()
        .map(|seconds| chrono::Utc::now() + chrono::Duration::seconds(seconds));

    let access_cipher = crate::util::token_crypto::encrypt_versioned(access.as_bytes())
        .map_err(RefreshFailure::Internal)?;
    let refresh_cipher = refresh
        .map(|token| crate::util::token_crypto::encrypt_versioned(token.as_bytes()))
        .transpose()
        .map_err(RefreshFailure::Internal)?;
    repo.store_tokens(
        row.id,
        &access_cipher,
        refresh_cipher.as_deref(),
        expires_at,
    )
    .await
    .map_err(RefreshFailure::Internal)?;
    Ok(expires_at)
}

async fn audit_refresh(
    state: &AppState,
    ctx: &AuthContext,
    row: &crate::models::BrokerCredential,
    outcome: &str,
    mut detail: serde_json::Value,
) -> Result<(), AppError> {
    detail["provider"] = json!(row.provider);
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerRefresh,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            outcome,
            detail,
        )
        .await
        .map_err(AppError::Internal)
}

/// DELETE /api/v1/broker/connections/:id
///
/// Attempts upstream revocation first, then deletes the local row
/// unconditionally. Per md/design/identity-broker.md S5, an upstream failure is
/// audited (`token_broker_revoke_upstream_failed`) but does not block deletion:
/// leaving the local row behind would strand the operator with a connection they
/// asked to remove and cannot remove. The audit row is the record that a
/// credential may still be live at the provider and needs manual revocation
/// there.
pub async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let repo = BrokerCredentialRepo::new(state.pool.clone());
    let row = repo
        .find_for_user(ctx.user_id, id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("broker connection not found".into()))?;

    let upstream = revoke_upstream(&state, &row).await;

    repo.delete(ctx.user_id, id)
        .await
        .map_err(AppError::Internal)?;

    let writer = IdentityAuditWriter::new(&state.pool);
    if let Err(reason) = &upstream {
        writer
            .write(
                IdentityEvent::TokenBrokerRevokeUpstreamFailed,
                ctx.org_id,
                Some(ctx.user_id),
                ctx.session_id,
                None,
                None,
                "failure",
                json!({"id": id, "provider": row.provider, "failureReason": reason}),
            )
            .await
            .map_err(AppError::Internal)?;
    }
    writer
        .write(
            IdentityEvent::TokenBrokerRevoke,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            json!({
                "id": id,
                "provider": row.provider,
                "upstreamRevoked": matches!(upstream, Ok(true)),
            }),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Revoke at the provider per RFC 7009. Revoking the refresh token invalidates
/// the whole grant (§2.1), so it is preferred over the access token when both
/// are held. `Ok(false)` means revocation was not attempted — the provider
/// registry declares no revocation endpoint, or there is nothing to revoke.
async fn revoke_upstream(
    state: &AppState,
    row: &crate::models::BrokerCredential,
) -> Result<bool, String> {
    let provider = load_provider(&row.provider).map_err(|e| e.to_string())?;
    let Some(revoke_url) = provider.revoke_url else {
        return Ok(false);
    };
    let (ciphertext, hint) = match (
        row.refresh_token_ciphertext.as_deref(),
        row.access_token_ciphertext.as_deref(),
    ) {
        (Some(refresh), _) => (refresh, "refresh_token"),
        (None, Some(access)) => (access, "access_token"),
        (None, None) => return Ok(false),
    };
    let token = decrypt_to_string(ciphertext).map_err(|e| format!("decrypt failed: {e}"))?;
    state
        .http
        .post(&revoke_url)
        .form(&[
            ("token", token.as_str()),
            ("token_type_hint", hint),
            ("client_id", "ione"),
        ])
        .send()
        .await
        .map_err(|e| format!("revocation request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("revocation endpoint returned {e}"))?;
    Ok(true)
}

fn decrypt_to_string(ciphertext: &[u8]) -> anyhow::Result<String> {
    let plaintext = crate::util::token_crypto::decrypt_versioned(ciphertext)?;
    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("token plaintext is not utf-8: {e}"))
}

struct Provider {
    authorize_url: String,
    token_url: String,
    /// RFC 7009 revocation endpoint. `None` when the provider does not publish
    /// one, in which case `DELETE` removes the local row and audits that no
    /// upstream revocation was possible.
    revoke_url: Option<String>,
    scopes_required: Vec<String>,
}

/// v0.1 provider registry: one generic, env-driven entry. Provider-specific
/// adapters (QuickBooks, Google Workspace) are deferred to v0.2 per
/// md/design/identity-broker.md; the mechanism here is complete, the catalogue
/// is deliberately not.
fn load_provider(name: &str) -> Result<Provider, AppError> {
    if name != "generic-test" {
        return Err(AppError::BadRequest("unknown broker provider".into()));
    }
    Ok(Provider {
        authorize_url: std::env::var("IONE_TEST_AUTHORIZE_URL")
            .unwrap_or_else(|_| "http://localhost:3901/authorize".into()),
        token_url: std::env::var("IONE_TEST_TOKEN_URL")
            .unwrap_or_else(|_| "http://localhost:3901/token".into()),
        revoke_url: std::env::var("IONE_TEST_REVOKE_URL").ok(),
        scopes_required: Vec::new(),
    })
}

// ── Brokered delegated tokens per (workspace, peer) — issue #12 ───────────────

/// POST /api/v1/workspaces/:id/peers/:peerId/delegation
///
/// Begin the delegation dance. The operator visits `authorizeUrl` once; from
/// then on every outbound MCP request made in this workspace's scope carries the
/// delegated token, with no further login at the peer.
pub async fn begin_peer_delegation(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::routes::peers::ensure_workspace_and_peer_in_org(
        &state,
        workspace_id,
        peer_id,
        ctx.org_id,
    )
    .await?;
    crate::auth::require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let granted_by = (!ctx.user_id.is_nil()).then_some(ctx.user_id);
    let authorize_url =
        crate::services::peer_delegation::begin(&state, workspace_id, peer_id, granted_by).await?;
    Ok(Json(json!({ "authorizeUrl": authorize_url })))
}

/// GET /auth/broker/peer-callback — public, like every OAuth callback. The
/// single-use `state` nonce is the only authenticator; it is consumed
/// atomically, so a replay finds nothing.
pub async fn peer_callback(
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
    let delegated = crate::services::peer_delegation::complete(&state, &state_token, &code).await?;
    Ok(Redirect::to(&format!(
        "/#/workspaces/{}/peers/{}",
        delegated.workspace_id, delegated.peer_id
    )))
}

/// GET /api/v1/workspaces/:id/peers/:peerId/delegation — metadata only; no
/// response surface returns delegated token material.
pub async fn get_peer_delegation(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::routes::peers::ensure_workspace_and_peer_in_org(
        &state,
        workspace_id,
        peer_id,
        ctx.org_id,
    )
    .await?;
    crate::auth::require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let delegation = crate::repos::WorkspacePeerDelegationRepo::new(state.pool.clone())
        .get(workspace_id, peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer delegation not found".into()))?;
    Ok(Json(
        serde_json::to_value(delegation).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

/// DELETE /api/v1/workspaces/:id/peers/:peerId/delegation — outbound auth for
/// this workspace falls back to the next precedence tier on the next request.
pub async fn revoke_peer_delegation(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    crate::routes::peers::ensure_workspace_and_peer_in_org(
        &state,
        workspace_id,
        peer_id,
        ctx.org_id,
    )
    .await?;
    crate::auth::require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let removed = crate::repos::WorkspacePeerDelegationRepo::new(state.pool.clone())
        .delete(workspace_id, peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    if !removed {
        return Err(AppError::NotFound("peer delegation not found".into()));
    }
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TokenBrokerRevoke,
            ctx.org_id,
            (!ctx.user_id.is_nil()).then_some(ctx.user_id),
            ctx.session_id,
            Some(peer_id),
            None,
            "success",
            json!({
                "scope": "workspace_peer",
                "workspaceId": workspace_id,
                "peerId": peer_id,
            }),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
