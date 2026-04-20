use axum::{
    extract::{Path, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::WorkspaceLifecycle,
    repos::{RoleRepo, WorkspaceRepo},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub domain: String,
    pub lifecycle: WorkspaceLifecycle,
    pub parent_id: Option<Uuid>,
}

pub async fn list_workspaces(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    // Derive org_id from the default user's org via workspace list scoped to that org.
    // For now all workspaces belong to the default org; org context comes from AppState
    // in later phases. We list all workspaces scoped to the default org.
    let org_id = get_default_org_id(&state).await?;
    let repo = WorkspaceRepo::new(state.pool.clone());
    let items = repo.list(org_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn create_workspace(
    State(state): State<AppState>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<Value>, AppError> {
    let org_id = get_default_org_id(&state).await?;
    let repo = WorkspaceRepo::new(state.pool.clone());
    let ws = repo
        .create(org_id, &req.name, &req.domain, req.lifecycle, req.parent_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn get_workspace(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    let ws = repo
        .get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("workspace {} not found", id)))?;
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn close_workspace(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    // Verify workspace exists first
    repo.get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("workspace {} not found", id)))?;

    let ws = repo.close(id).await.map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn list_roles(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = RoleRepo::new(state.pool.clone());
    let items = repo.list(workspace_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

/// Resolve the default org_id by looking up the user's org.
async fn get_default_org_id(state: &AppState) -> Result<Uuid, AppError> {
    sqlx::query_scalar::<_, Uuid>("SELECT org_id FROM users WHERE id = $1")
        .bind(state.default_user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to resolve org_id: {}", e)))
}
