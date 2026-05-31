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
    repos::{PeerRepo, RoleRepo, WorkspaceRepo},
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
    let mut body = serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?;
    if let Value::Object(map) = &mut body {
        map.insert(
            "panels".to_string(),
            workspace_panels(&state.pool, id).await?,
        );
    }
    Ok(Json(body))
}

/// Per-panel data-presence summary driving the adaptive nav. Cheap COUNT/EXISTS
/// queries only — no peer fan-out. `hasActivePeer` is the presence proxy for the
/// federation-only Map/Document panels (and inclusively surfaces peer-fed
/// charts/tables); native chart/table streams are counted directly.
async fn workspace_panels(pool: &sqlx::PgPool, workspace_id: Uuid) -> Result<Value, AppError> {
    let charts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1 AND s.view_config IS NOT NULL",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1 AND jsonb_exists(s.view_config, 'property_fields')",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let has_active_peer: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM workspace_peer_bindings b
            JOIN peers p ON p.id = b.peer_id
            WHERE b.workspace_id = $1 AND b.status = 'active' AND p.status = 'active'
         )",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let approvals_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1 AND ap.status = 'pending'",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(json!({
        "charts": charts,
        "tables": tables,
        "hasActivePeer": has_active_peer,
        "approvalsPending": approvals_pending,
    }))
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

pub async fn list_peer_tools(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let manifest =
        crate::services::federation::workspace_peer_manifest(&state, workspace_id, peer_id, &ctx)
            .await
            .map_err(AppError::Internal)?;
    let peer = PeerRepo::new(state.pool.clone())
        .get_for_org(peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer not found".into()))?;
    let tools = crate::services::federation::namespaced_tools_from_manifest(&peer, &manifest);
    Ok(Json(json!({
        "peerId": peer_id,
        "stale": manifest.stale,
        "fetchedAt": manifest.fetched_at,
        "items": tools,
    })))
}

pub async fn list_peer_resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let body =
        crate::services::federation::workspace_peer_resources(&state, workspace_id, peer_id, &ctx)
            .await
            .map_err(AppError::Internal)?;
    Ok(Json(body))
}

pub async fn list_context_slices(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let items = crate::services::federation::workspace_context_slices(&state, workspace_id, &ctx)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
