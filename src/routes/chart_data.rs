use axum::{
    extract::{Path, Query, State},
    response::Json,
    Extension,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::WorkspacePeerBindingRepo,
    services::chart_data::{fetch_chart_data, ChartDataResponse},
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartDataQuery {
    #[serde(alias = "peer_id")]
    peer_id: Option<Uuid>,
    uri: Option<String>,
}

pub async fn get_chart_data(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ChartDataQuery>,
) -> Result<Json<ChartDataResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let peer_id = query
        .peer_id
        .ok_or_else(|| AppError::BadRequest("peer_id is required".into()))?;
    let uri = query
        .uri
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("uri is required".into()))?;

    let peers = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_active_peers_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    let peer = peers
        .into_iter()
        .find(|peer| peer.id == peer_id)
        .ok_or_else(|| AppError::NotFound("peer not bound to workspace".into()))?;

    let response = fetch_chart_data(&state, &peer, &uri)
        .await
        .map_err(|err| AppError::ConnectorError(err.to_string()))?;
    Ok(Json(response))
}
