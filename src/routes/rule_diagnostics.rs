use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::RuleDiagnosticsRepo,
    state::AppState,
};

pub async fn get_diagnostics(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let snap = RuleDiagnosticsRepo::new(state.pool.clone())
        .get(workspace_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({
        "evaluatedAt": snap.as_ref().map(|s| s.0),
        "items": snap.map(|s| s.1).unwrap_or_default(),
    })))
}
