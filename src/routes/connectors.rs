use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    connectors,
    error::AppError,
    middleware::session_cookie::SessionId,
    models::{
        ActivationStepKey, ActivationTrack, ConnectorKind, PipelineEventInput, PipelineEventStage,
    },
    repos::{ConnectorRepo, PipelineEventRepo, StreamEventRepo, StreamRepo},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConnectorRequest {
    pub kind: ConnectorKind,
    pub name: String,
    pub config: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateBody {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub config: Value,
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
    Extension(auth): Extension<AuthContext>,
    Extension(session): Extension<SessionId>,
    Json(req): Json<CreateConnectorRequest>,
) -> Response {
    let kind = match &req.kind {
        ConnectorKind::Mcp => "mcp",
        ConnectorKind::Openapi => "openapi",
        ConnectorKind::RustNative => "rust_native",
    };

    match crate::connectors::validate::dispatch(kind, &req.name, &req.config).await {
        Ok(_) => {}
        Err(err) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(err)).into_response();
        }
    }

    match do_create_connector(state, auth, session, workspace_id, req).await {
        Ok(resp) => resp.into_response(),
        Err(err) => err.into_response(),
    }
}

pub(crate) async fn validate_connector(
    State(state): State<AppState>,
    Extension(session): Extension<SessionId>,
    Json(body): Json<ValidateBody>,
) -> Response {
    crate::services::funnel::track(
        &state,
        session.0,
        None,
        None,
        "connector_validate_attempted",
        Some(json!({ "kind": body.kind })),
    );

    match crate::connectors::validate::dispatch(&body.kind, &body.name, &body.config).await {
        Ok(ok) => {
            crate::services::funnel::track(
                &state,
                session.0,
                None,
                None,
                "connector_validate_succeeded",
                Some(json!({ "kind": body.kind })),
            );
            (StatusCode::OK, Json(ok)).into_response()
        }
        Err(err) => {
            crate::services::funnel::track(
                &state,
                session.0,
                None,
                None,
                "connector_validate_failed",
                Some(json!({ "kind": body.kind, "errorKind": err.error })),
            );
            (StatusCode::UNPROCESSABLE_ENTITY, Json(err)).into_response()
        }
    }
}

async fn do_create_connector(
    state: AppState,
    auth: AuthContext,
    session: SessionId,
    workspace_id: Uuid,
    req: CreateConnectorRequest,
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
    let impl_ = connectors::build(req.kind.clone(), &req.name, &req.config)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let default_streams = impl_
        .default_streams()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let connector = connector_repo
        .create(workspace_id, req.kind, &req.name, req.config)
        .await
        .map_err(AppError::Internal)?;
    crate::services::funnel::track(
        &state,
        session.0,
        Some(auth.user_id),
        Some(workspace_id),
        "connector_created",
        Some(json!({ "kind": connector.kind })),
    );

    let connector_json =
        serde_json::to_value(&connector).map_err(|e| AppError::Internal(e.into()))?;

    emit_pipeline_stage(
        &state,
        workspace_id,
        Some(connector.id),
        None,
        PipelineEventStage::PublishStarted,
        None,
    )
    .await;

    let mut streams = Vec::new();
    for sd in default_streams {
        let stream = stream_repo
            .upsert_named(connector.id, &sd.name, sd.schema)
            .await
            .map_err(AppError::Internal)?;
        streams.push(stream);
    }

    run_initial_connector_poll(&state, workspace_id, connector.id, impl_.as_ref(), streams).await;

    if workspace_id != crate::demo::DEMO_WORKSPACE_ID {
        let activation_repo = crate::repos::ActivationRepo::new(state.pool.clone());
        let inserted = activation_repo
            .mark(
                auth.user_id,
                workspace_id,
                ActivationTrack::RealActivation,
                ActivationStepKey::AddedConnector,
            )
            .await
            .unwrap_or(false);
        if inserted
            && activation_repo
                .is_track_complete(auth.user_id, workspace_id, ActivationTrack::RealActivation)
                .await
                .unwrap_or(false)
        {
            crate::services::funnel::track(
                &state,
                session.0,
                Some(auth.user_id),
                Some(workspace_id),
                "activation_completed",
                Some(json!({ "track": "real_activation" })),
            );
        }
    }

    Ok(Json(connector_json))
}

async fn run_initial_connector_poll(
    state: &AppState,
    workspace_id: Uuid,
    connector_id: Uuid,
    connector: &dyn connectors::ConnectorImpl,
    streams: Vec<crate::models::Stream>,
) {
    let event_repo = StreamEventRepo::new(state.pool.clone());
    let mut first_event_emitted = false;

    for stream in streams {
        let cursor = match event_repo.latest_observed_at(stream.id).await {
            Ok(cursor) => cursor.map(|dt| json!({ "observed_at": dt.to_rfc3339() })),
            Err(err) => {
                emit_pipeline_error(
                    state,
                    workspace_id,
                    Some(connector_id),
                    Some(stream.id),
                    "poll_cursor",
                    &err,
                )
                .await;
                warn!(
                    connector_id = %connector_id,
                    stream_id = %stream.id,
                    error = %err,
                    "failed to load stream cursor during connector create"
                );
                continue;
            }
        };

        let poll_result = match connector.poll(&stream.name, cursor).await {
            Ok(result) => result,
            Err(err) => {
                emit_pipeline_error(
                    state,
                    workspace_id,
                    Some(connector_id),
                    Some(stream.id),
                    "poll",
                    &err,
                )
                .await;
                warn!(
                    connector_id = %connector_id,
                    stream = %stream.name,
                    error = %err,
                    "connector initial poll failed"
                );
                continue;
            }
        };

        let mut inserted_count = 0usize;
        for evt in poll_result.events {
            match event_repo
                .insert_if_absent(stream.id, evt.payload, evt.observed_at)
                .await
            {
                Ok(true) => inserted_count += 1,
                Ok(false) => {}
                Err(err) => {
                    emit_pipeline_error(
                        state,
                        workspace_id,
                        Some(connector_id),
                        Some(stream.id),
                        "stream_event_insert",
                        &err,
                    )
                    .await;
                    warn!(
                        connector_id = %connector_id,
                        stream_id = %stream.id,
                        error = %err,
                        "failed to insert stream event during connector create"
                    );
                    break;
                }
            }
        }

        if inserted_count > 0 && !first_event_emitted {
            emit_pipeline_stage(
                state,
                workspace_id,
                Some(connector_id),
                Some(stream.id),
                PipelineEventStage::FirstEvent,
                Some(json!({ "event_count": inserted_count })),
            )
            .await;
            first_event_emitted = true;
        }
    }
}

async fn emit_pipeline_stage(
    state: &AppState,
    workspace_id: Uuid,
    connector_id: Option<Uuid>,
    stream_id: Option<Uuid>,
    stage: PipelineEventStage,
    detail: Option<Value>,
) {
    let repo = PipelineEventRepo::new(state.pool.clone());
    let input = PipelineEventInput {
        workspace_id,
        connector_id,
        stream_id,
        stage,
        detail,
    };

    match repo.append(input).await {
        Ok(event) => state.pipeline_bus.publish(event),
        Err(err) => warn!(
            workspace_id = %workspace_id,
            connector_id = ?connector_id,
            stream_id = ?stream_id,
            stage = stage.as_str(),
            error = %err,
            "pipeline event append failed during connector create"
        ),
    }
}

async fn emit_pipeline_error(
    state: &AppState,
    workspace_id: Uuid,
    connector_id: Option<Uuid>,
    stream_id: Option<Uuid>,
    stage_name: &str,
    error: impl ToString,
) {
    emit_pipeline_stage(
        state,
        workspace_id,
        connector_id,
        stream_id,
        PipelineEventStage::Error,
        Some(json!({
            "stage": stage_name,
            "error": error.to_string(),
        })),
    )
    .await;
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
    let cursor = event_repo
        .latest_observed_at(stream_id)
        .await
        .map_err(AppError::Internal)?
        .map(|dt| json!({ "observed_at": dt.to_rfc3339() }));

    let poll_result = match impl_.poll(&stream.name, cursor).await {
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
