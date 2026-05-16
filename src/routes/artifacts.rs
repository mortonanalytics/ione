use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::ArtifactRepo,
    state::AppState,
};

/// GET /api/v1/workspaces/:id/artifacts
pub async fn list_artifacts(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let repo = ArtifactRepo::new(state.pool.clone());
    let items = repo
        .list(workspace_id, 100)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
