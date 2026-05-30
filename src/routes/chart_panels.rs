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
    services::chart_panels::{fetch_chart_panels, ChartPanelsResponse},
    state::AppState,
};

pub async fn list_chart_panels(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<ChartPanelsResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let peers = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_active_peers_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;

    let response = fetch_chart_panels(&state.pool, &state, workspace_id, ctx.org_id, peers)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(response))
}
