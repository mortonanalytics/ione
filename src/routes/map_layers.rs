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
    services::map_layers::{fetch_map_layers, MapLayersResponse},
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLayersQuery {
    #[serde(alias = "peer_id")]
    pub peer_id: Option<Uuid>,
}

pub async fn list_map_layers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<MapLayersQuery>,
) -> Result<Json<MapLayersResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let peers = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_active_peers_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;

    let response = fetch_map_layers(&state.http, peers, query.peer_id).await;
    Ok(Json(response))
}
