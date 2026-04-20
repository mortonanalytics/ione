use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    models::{Severity, SignalSource},
    repos::SignalRepo,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct ListSignalsQuery {
    pub source: Option<String>,
    pub severity: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_signals(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ListSignalsQuery>,
) -> Result<Json<Value>, AppError> {
    let limit = query.limit.unwrap_or(100).min(500);

    let source_filter = query.source.as_deref().and_then(parse_signal_source);
    let severity_filter = query.severity.as_deref().and_then(parse_severity);

    let repo = SignalRepo::new(state.pool.clone());
    let items = repo
        .list(workspace_id, source_filter, severity_filter, limit)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({ "items": items })))
}

fn parse_signal_source(s: &str) -> Option<SignalSource> {
    match s {
        "rule" => Some(SignalSource::Rule),
        "connector_event" => Some(SignalSource::ConnectorEvent),
        "generator" => Some(SignalSource::Generator),
        _ => None,
    }
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s {
        "routine" => Some(Severity::Routine),
        "flagged" => Some(Severity::Flagged),
        "command" => Some(Severity::Command),
        _ => None,
    }
}
