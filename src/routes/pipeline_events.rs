use axum::{
    extract::{Path, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListQuery {
    pub connector_id: Option<Uuid>,
    pub stage: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    pub cursor: Option<DateTime<Utc>>,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResp {
    pub items: Vec<crate::models::PipelineEvent>,
    pub next_cursor: Option<DateTime<Utc>>,
}

pub(crate) async fn list_events(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResp>, AppError> {
    use crate::models::PipelineEventStage as Stage;

    let stage = match q.stage.as_deref() {
        Some("publish_started") => Some(Stage::PublishStarted),
        Some("first_event") => Some(Stage::FirstEvent),
        Some("first_signal") => Some(Stage::FirstSignal),
        Some("first_survivor") => Some(Stage::FirstSurvivor),
        Some("first_decision") => Some(Stage::FirstDecision),
        Some("stall") => Some(Stage::Stall),
        Some("error") => Some(Stage::Error),
        Some(_) | None => None,
    };

    let repo = crate::repos::PipelineEventRepo::new(state.pool.clone());
    let filter = crate::repos::EventFilter {
        connector_id: q.connector_id,
        stage,
        limit: q.limit,
        before: q.cursor,
    };
    let (items, next_cursor) = repo
        .list(workspace_id, filter)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(ListResp { items, next_cursor }))
}

pub(crate) async fn stream_events(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = state
        .pipeline_bus
        .subscribe_workspace(workspace_id)
        .map(|ev| {
            let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
            Ok(Event::default().event("pipeline_event").data(data))
        });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
