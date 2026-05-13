use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    Extension,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::{require_admin, AuthContext},
    error::AppError,
    repos::TrustIssuerRepo,
    services::{IdentityAuditWriter, IdentityEvent},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTrustIssuerBody {
    pub idp_type: String,
    pub issuer_url: String,
    pub audience: String,
    pub jwks_uri: String,
    pub claim_mapping: Value,
    pub max_coc_level: i32,
    pub client_secret: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustIssuerResp {
    pub id: Uuid,
    pub idp_type: String,
    pub issuer_url: String,
    pub audience: String,
    pub jwks_uri: String,
    pub max_coc_level: i32,
    pub claim_mapping: Value,
    pub display_name: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<TrustIssuerResp>>, AppError> {
    require_admin(&ctx, &state.pool).await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let rows = TrustIssuerRepo::new(state.pool.clone())
        .list(ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(rows.into_iter().map(TrustIssuerResp::from).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<CreateTrustIssuerBody>,
) -> Result<Json<TrustIssuerResp>, AppError> {
    require_admin(&ctx, &state.pool).await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    if body.idp_type != "oidc" {
        return Err(AppError::BadRequest("idp_type must be oidc".into()));
    }
    if !body.issuer_url.starts_with("https://") && !body.issuer_url.starts_with("http://localhost")
    {
        return Err(AppError::BadRequest("issuer_url must be https".into()));
    }
    if !(0..=100).contains(&body.max_coc_level) {
        return Err(AppError::BadRequest("max_coc_level must be 0..=100".into()));
    }
    let secret = body
        .client_secret
        .as_deref()
        .map(|s| crate::util::token_crypto::encrypt_versioned(s.as_bytes()))
        .transpose()
        .map_err(AppError::Internal)?;
    let row = TrustIssuerRepo::new(state.pool.clone())
        .create_oidc(
            ctx.org_id,
            &body.issuer_url,
            &body.audience,
            &body.jwks_uri,
            body.claim_mapping,
            body.max_coc_level,
            secret,
            body.display_name,
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("duplicate") || e.to_string().contains("unique") {
                AppError::BadRequest("duplicate trust issuer".into())
            } else {
                AppError::Internal(e)
            }
        })?;
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TrustIssuerCreate,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            serde_json::json!({"issuer_url": row.issuer_url, "audience": row.audience, "idp_type": row.idp_type}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(TrustIssuerResp::from(row)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    require_admin(&ctx, &state.pool).await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let rows = TrustIssuerRepo::new(state.pool.clone())
        .delete(ctx.org_id, id)
        .await
        .map_err(AppError::Internal)?;
    if rows == 0 {
        return Err(AppError::NotFound("trust issuer not found".into()));
    }
    IdentityAuditWriter::new(&state.pool)
        .write(
            IdentityEvent::TrustIssuerDelete,
            ctx.org_id,
            Some(ctx.user_id),
            ctx.session_id,
            None,
            None,
            "success",
            serde_json::json!({"id": id}),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<crate::models::TrustIssuer> for TrustIssuerResp {
    fn from(ti: crate::models::TrustIssuer) -> Self {
        Self {
            id: ti.id,
            idp_type: ti.idp_type,
            issuer_url: ti.issuer_url,
            audience: ti.audience,
            jwks_uri: ti.jwks_uri,
            max_coc_level: ti.max_coc_level,
            claim_mapping: ti.claim_mapping,
            display_name: ti.display_name,
        }
    }
}
