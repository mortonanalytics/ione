use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{error::AppError, models::CriticVerdict, repos::SurvivorRepo, state::AppState};

#[derive(Debug, Deserialize)]
pub struct ListSurvivorsQuery {
    pub verdict: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_survivors(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ListSurvivorsQuery>,
) -> Result<Json<Value>, AppError> {
    let limit = query.limit.unwrap_or(100).min(500);

    let verdict_filter = query.verdict.as_deref().and_then(parse_verdict);

    let repo = SurvivorRepo::new(state.pool.clone());
    let items = repo
        .list(workspace_id, verdict_filter, limit)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({ "items": items })))
}

fn parse_verdict(s: &str) -> Option<CriticVerdict> {
    match s {
        "survive" => Some(CriticVerdict::Survive),
        "reject" => Some(CriticVerdict::Reject),
        "defer" => Some(CriticVerdict::Defer),
        _ => None,
    }
}
