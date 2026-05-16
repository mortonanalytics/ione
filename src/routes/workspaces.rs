use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    middleware::session_cookie::SessionId,
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

pub async fn list_workspaces(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    let items = repo.list(ctx.org_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn create_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Extension(session): Extension<SessionId>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    if let Some(parent_id) = req.parent_id {
        ensure_workspace_in_org(&state.pool, parent_id, ctx.org_id).await?;
    }
    let ws = repo
        .create(
            ctx.org_id,
            &req.name,
            &req.domain,
            req.lifecycle,
            req.parent_id,
        )
        .await
        .map_err(AppError::Internal)?;
    if ws.id != crate::demo::DEMO_WORKSPACE_ID {
        crate::services::funnel::track(
            &state,
            session.0,
            Some(ctx.user_id),
            Some(ws.id),
            "real_workspace_created",
            Some(json!({ "workspaceId": ws.id })),
        );
    }
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn get_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, id, ctx.org_id).await?;
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
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, id, ctx.org_id).await?;
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
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let repo = RoleRepo::new(state.pool.clone());
    let items = repo.list(workspace_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
