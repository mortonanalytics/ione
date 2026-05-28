use axum::{
    extract::{Path, Query, State},
    response::Json,
    Extension,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    repos::StreamEventRepo,
    services::event_layers::{project_event_layers, EventLayersResponse},
    state::AppState,
};

const DEFAULT_LIMIT: i64 = 5000;
const MAX_LIMIT: i64 = 5000;
const MAX_WINDOW_DAYS: i64 = 30;
const DEFAULT_WINDOW_HOURS: i64 = 24;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLayersQuery {
    #[serde(default, alias = "since")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, alias = "until")]
    pub until: Option<DateTime<Utc>>,
    #[serde(default, alias = "stream_id")]
    pub stream_id: Option<Uuid>,
    #[serde(default, alias = "limit")]
    pub limit: Option<i64>,
}

pub async fn list_event_layers(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<EventLayersQuery>,
) -> Result<Json<EventLayersResponse>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;

    let queried_at = Utc::now();
    let until = query.until.unwrap_or(queried_at);
    let since = query
        .since
        .unwrap_or_else(|| queried_at - Duration::hours(DEFAULT_WINDOW_HOURS));

    if since > until {
        return Err(AppError::BadRequest("since must be <= until".to_string()));
    }
    if until - since > Duration::days(MAX_WINDOW_DAYS) {
        return Err(AppError::BadRequest(format!(
            "time window must be <= {MAX_WINDOW_DAYS} days"
        )));
    }

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }

    let (catalog, events) = StreamEventRepo::new(state.pool.clone())
        .fetch_geo_events(
            workspace_id,
            ctx.org_id,
            query.stream_id,
            since,
            until,
            limit,
        )
        .await
        .map_err(AppError::Internal)?;

    let response = project_event_layers(catalog, events, limit, queried_at);
    Ok(Json(response))
}
