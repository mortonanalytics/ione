use axum::{
    extract::{Path, State},
    response::Json,
    Extension,
};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::WorkspacePeerBindingRepo,
    services::table_panels::{fetch_table_panels, TablePanelsResponse},
    state::AppState,
};

pub async fn list_table_panels(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<TablePanelsResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let peers = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_active_peers_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;

    let response = fetch_table_panels(&state.pool, &state.http, workspace_id, ctx.org_id, peers)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(response))
}
