use axum::{
    extract::{Path, State},
    response::Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{error::AppError, repos::ArtifactRepo, state::AppState};

/// GET /api/v1/workspaces/:id/artifacts
pub async fn list_artifacts(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = ArtifactRepo::new(state.pool.clone());
    let items = repo
        .list(workspace_id, 100)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
