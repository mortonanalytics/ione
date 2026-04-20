use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{error::AppError, repos::RoutingDecisionRepo, state::AppState};

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    #[serde(rename = "roleId")]
    pub role_id: Option<Uuid>,
    pub limit: Option<i64>,
}

pub async fn get_feed(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<Value>, AppError> {
    let role_id = match query.role_id {
        Some(id) => id,
        None => {
            return Err(AppError::BadRequest(
                "roleId query parameter is required".to_string(),
            ))
        }
    };

    let limit = query.limit.unwrap_or(100).min(500);

    let repo = RoutingDecisionRepo::new(state.pool.clone());
    let items = repo
        .feed_for_role(workspace_id, role_id, limit)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({ "items": items })))
}
