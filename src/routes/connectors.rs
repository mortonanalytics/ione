use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tracing::warn;
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    connectors,
    error::AppError,
    middleware::session_cookie::SessionId,
    models::{
        ActivationStepKey, ActivationTrack, ActorKind, ConnectorKind, PipelineEventInput,
        PipelineEventStage,
    },
    repos::{ConnectorRepo, InsertOutcome, PipelineEventRepo, StreamEventRepo, StreamRepo},
    services::event_layers::validate_view_config,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PutViewConfigResponse {
    pub id: Uuid,
    pub view_config: Value,
}

pub async fn list_connectors(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;
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
    if let Err(err) = ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await {
        return err.into_response();
    }
    // HP-H1: create_connector was the lone composing endpoint missing the
    // workspace:write gate that patch_workspace already enforces.
    if let Err(err) =
        require_permission(&auth, &state.pool, workspace_id, "workspace:write").await
    {
        return err.into_response();
    }

    let kind = match &req.kind {
        ConnectorKind::Mcp => "mcp",
        ConnectorKind::Openapi => "openapi",
        ConnectorKind::RustNative => "rust_native",
    };

    if matches!(req.kind, ConnectorKind::RustNative) {
        match crate::connectors::validate::dispatch(kind, &req.name, &req.config).await {
            Ok(_) => {}
            Err(err) => {
                return (StatusCode::UNPROCESSABLE_ENTITY, Json(err)).into_response();
            }
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
            .upsert_named(connector.id, &sd.name, sd.schema, sd.view_config)
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
                .insert_event(
                    stream.id,
                    evt.payload,
                    evt.observed_at,
                    evt.dedup_key.as_deref(),
                )
                .await
            {
                Ok(InsertOutcome::Inserted) => inserted_count += 1,
                Ok(InsertOutcome::Updated | InsertOutcome::Duplicate) => {}
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
    Extension(ctx): Extension<AuthContext>,
    Path(connector_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = StreamRepo::new(state.pool.clone());
    let items = repo
        .list_in_org(connector_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn poll_stream(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(stream_id): Path<Uuid>,
) -> Response {
    match do_poll_stream(state, ctx.org_id, stream_id).await {
        Ok(resp) => resp.into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn put_stream_view_config(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(stream_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Result<Json<PutViewConfigResponse>, AppError> {
    validate_view_config(&body).map_err(AppError::UnprocessableEntity)?;

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|err| AppError::Internal(err.into()))?;

    let stream = sqlx::query(
        "SELECT s.view_config, c.workspace_id
         FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         JOIN workspaces w ON w.id = c.workspace_id
         WHERE s.id = $1 AND w.org_id = $2",
    )
    .bind(stream_id)
    .bind(ctx.org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| AppError::Internal(err.into()))?
    .ok_or_else(|| AppError::NotFound("stream not found".into()))?;

    let old_view_config: Option<Value> = stream
        .try_get("view_config")
        .map_err(|err| AppError::Internal(err.into()))?;
    let workspace_id: Uuid = stream
        .try_get("workspace_id")
        .map_err(|err| AppError::Internal(err.into()))?;
    let old_hash = old_view_config.as_ref().map(stable_json_hash);

    let updated_id: Uuid = sqlx::query_scalar(
        "UPDATE streams
         SET view_config = $2
         WHERE id = $1
         RETURNING id",
    )
    .bind(stream_id)
    .bind(body.clone())
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| AppError::Internal(err.into()))?;
    let new_hash = stable_json_hash(&body);

    sqlx::query(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, object_id, payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(workspace_id)
    .bind(ActorKind::User)
    .bind(ctx.user_id.to_string())
    .bind("stream.view_config.updated")
    .bind("stream")
    .bind(stream_id)
    .bind(json!({
        "old_hash": old_hash,
        "new_hash": new_hash,
    }))
    .execute(&mut *tx)
    .await
    .map_err(|err| AppError::Internal(err.into()))?;

    tx.commit()
        .await
        .map_err(|err| AppError::Internal(err.into()))?;

    Ok(Json(PutViewConfigResponse {
        id: updated_id,
        view_config: body,
    }))
}

async fn do_poll_stream(
    state: AppState,
    org_id: Uuid,
    stream_id: Uuid,
) -> Result<Json<Value>, AppError> {
    let stream_repo = StreamRepo::new(state.pool.clone());
    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let event_repo = StreamEventRepo::new(state.pool.clone());

    // Look up the stream
    let stream = stream_repo
        .get_in_org(stream_id, org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("stream not found".into()))?;

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
    let impl_ = connectors::build_from_row_with_pool(&connector, state.pool.clone())
        .map_err(AppError::Internal)?;
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
        let outcome = event_repo
            .insert_event(
                stream_id,
                evt.payload,
                evt.observed_at,
                evt.dedup_key.as_deref(),
            )
            .await
            .map_err(AppError::Internal)?;
        if matches!(outcome, InsertOutcome::Inserted) {
            ingested += 1;
        }
    }

    Ok(Json(json!({ "ingested": ingested })))
}

fn stable_json_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("serde_json::Value always serializes");
    hex::encode(Sha256::digest(bytes))
}
