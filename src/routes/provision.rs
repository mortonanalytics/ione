use axum::{extract::State, response::Json, Extension};
use serde_json::Value;

use crate::{
    auth::{require_org_permission, AuthContext},
    error::AppError,
    services::provisioning::{self, ProvisionSpec},
    state::AppState,
};

/// POST /api/v1/provision — apply a declarative workspace-graph spec.
/// Org-scoped, gated by `provisioning:apply`. Errors map to 422 (entity named)
/// or 409 (escalation); the whole apply is one transaction.
pub async fn provision(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(spec): Json<ProvisionSpec>,
) -> Result<Json<Value>, AppError> {
    require_org_permission(&ctx, &state.pool, "provisioning:apply").await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let result = provisioning::apply(&state, &ctx, spec).await?;
    Ok(Json(
        serde_json::to_value(result).map_err(|e| AppError::Internal(e.into()))?,
    ))
}
