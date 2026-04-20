use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    connectors,
    error::AppError,
    models::ConnectorKind,
    repos::{ConnectorRepo, StreamEventRepo, StreamRepo},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConnectorRequest {
    pub kind: ConnectorKind,
    pub name: String,
    pub config: serde_json::Value,
}

pub async fn list_connectors(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = ConnectorRepo::new(state.pool.clone());
    let items = repo.list(workspace_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn create_connector(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateConnectorRequest>,
) -> Result<Json<Value>, AppError> {
    // Verify workspace exists
    let ws_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)")
            .bind(workspace_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("workspace lookup failed: {}", e)))?;

    if !ws_exists {
        return Err(AppError::BadRequest(format!(
            "workspace {} not found",
            workspace_id
        )));
    }

    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let stream_repo = StreamRepo::new(state.pool.clone());

    let connector = connector_repo
        .create(workspace_id, req.kind, &req.name, req.config)
        .await
        .map_err(AppError::Internal)?;

    // Auto-register default streams for this connector
    let impl_ = connectors::build_from_row(&connector).map_err(AppError::Internal)?;
    let default_streams = impl_.default_streams().await.map_err(AppError::Internal)?;

    for sd in default_streams {
        stream_repo
            .upsert_named(connector.id, &sd.name, sd.schema)
            .await
            .map_err(AppError::Internal)?;
    }

    Ok(Json(
        serde_json::to_value(connector).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn list_streams(
    State(state): State<AppState>,
    Path(connector_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = StreamRepo::new(state.pool.clone());
    let items = repo.list(connector_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn poll_stream(State(state): State<AppState>, Path(stream_id): Path<Uuid>) -> Response {
    match do_poll_stream(state, stream_id).await {
        Ok(resp) => resp.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn do_poll_stream(state: AppState, stream_id: Uuid) -> Result<Json<Value>, AppError> {
    let stream_repo = StreamRepo::new(state.pool.clone());
    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let event_repo = StreamEventRepo::new(state.pool.clone());

    // Look up the stream
    let stream = stream_repo
        .get(stream_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("stream {} not found", stream_id)))?;

    // Look up the connector
    let connector = connector_repo
        .get(stream.connector_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "connector {} not found for stream {}",
                stream.connector_id,
                stream_id
            ))
        })?;

    // Dispatch to the connector implementation
    let impl_ = connectors::build_from_row(&connector).map_err(AppError::Internal)?;

    let poll_result = match impl_.poll(&stream.name, None).await {
        Ok(r) => r,
        Err(e) => {
            // Record the error on the connector
            let _ = connector_repo
                .update_status(
                    connector.id,
                    crate::models::ConnectorStatus::Error,
                    Some(e.to_string().as_str()),
                )
                .await;
            return Err(AppError::ConnectorError(e.to_string()));
        }
    };

    // Insert events with dedup
    let mut ingested: i64 = 0;
    for evt in poll_result.events {
        let inserted = event_repo
            .insert_if_absent(stream_id, evt.payload, evt.observed_at)
            .await
            .map_err(AppError::Internal)?;
        if inserted {
            ingested += 1;
        }
    }

    Ok(Json(json!({ "ingested": ingested })))
}
