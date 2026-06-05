use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    middleware::session_cookie::SessionId,
    models::WorkspaceLifecycle,
    repos::{PeerRepo, RoleRepo, RuleDiagnosticsRepo, WorkspaceRepo},
    state::AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub domain: String,
    pub lifecycle: WorkspaceLifecycle,
    pub parent_id: Option<Uuid>,
}

pub async fn list_workspaces(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    let items = repo.list(ctx.org_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn create_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Extension(session): Extension<SessionId>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<Value>, AppError> {
    let repo = WorkspaceRepo::new(state.pool.clone());
    if let Some(parent_id) = req.parent_id {
        ensure_workspace_in_org(&state.pool, parent_id, ctx.org_id).await?;
    }
    let ws = repo
        .create(
            ctx.org_id,
            &req.name,
            &req.domain,
            req.lifecycle,
            req.parent_id,
        )
        .await
        .map_err(AppError::Internal)?;
    if ws.id != crate::demo::DEMO_WORKSPACE_ID {
        crate::services::funnel::track(
            &state,
            session.0,
            Some(ctx.user_id),
            Some(ws.id),
            "real_workspace_created",
            Some(json!({ "workspaceId": ws.id })),
        );
    }
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn get_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, id, ctx.org_id).await?;
    let repo = WorkspaceRepo::new(state.pool.clone());
    let ws = repo
        .get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("workspace {} not found", id)))?;
    let mut body = serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?;
    if let Value::Object(map) = &mut body {
        map.insert(
            "panels".to_string(),
            workspace_panels(&state.pool, id).await?,
        );
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct PatchWorkspaceRequest {
    pub metadata: Value,
}

#[derive(Deserialize)]
struct RuleShape {
    stream: String,
    when: String,
    severity: String,
    title: String,
}

/// PATCH /api/v1/workspaces/:id
///
/// Shallow-merges the supplied `metadata` object into `workspaces.metadata`,
/// the canonical home for `rules`, `default_map_center`, `product`, and (for the
/// app-onramp artifact path) `view_config` installs. Returns the workspace with
/// the recomputed `panels` summary, identical in shape to `GET`.
pub async fn patch_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchWorkspaceRequest>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, id, ctx.org_id).await?;
    if !req.metadata.is_object() {
        return Err(AppError::BadRequest(
            "metadata must be a JSON object".to_string(),
        ));
    }
    let updates_rules = req.metadata.get("rules").is_some();
    if let Some(rules) = req.metadata.get("rules") {
        let parsed: Vec<RuleShape> = serde_json::from_value(rules.clone()).map_err(|err| {
            AppError::UnprocessableEntityJson(json!({
                "error": "invalid_rules",
                "detail": err.to_string(),
            }))
        })?;
        for (i, rule) in parsed.iter().enumerate() {
            if rule.stream.trim().is_empty()
                || rule.when.trim().is_empty()
                || rule.severity.trim().is_empty()
                || rule.title.trim().is_empty()
            {
                return Err(AppError::UnprocessableEntityJson(json!({
                    "error": "invalid_rule",
                    "ruleIndex": i,
                    "detail": "stream, when, severity, and title are required",
                })));
            }
            if !matches!(rule.severity.as_str(), "routine" | "flagged" | "command") {
                return Err(AppError::UnprocessableEntityJson(json!({
                    "error": "invalid_rule",
                    "ruleIndex": i,
                    "detail": "severity must be routine, flagged, or command",
                })));
            }
            evalexpr::build_operator_tree(&rule.when).map_err(|err| {
                AppError::UnprocessableEntityJson(json!({
                    "error": "invalid_rule_expression",
                    "ruleIndex": i,
                    "detail": err.to_string(),
                }))
            })?;
        }
    }
    let repo = WorkspaceRepo::new(state.pool.clone());
    let ws = repo
        .update_metadata(id, &req.metadata)
        .await
        .map_err(AppError::Internal)?;
    if updates_rules {
        RuleDiagnosticsRepo::new(state.pool.clone())
            .clear(id)
            .await
            .map_err(AppError::Internal)?;
    }
    let mut body = serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?;
    if let Value::Object(map) = &mut body {
        map.insert(
            "panels".to_string(),
            workspace_panels(&state.pool, id).await?,
        );
    }
    Ok(Json(body))
}

/// Per-panel data-presence summary driving the adaptive nav. Cheap COUNT/EXISTS
/// queries only — no peer fan-out. `hasActivePeer` is the presence proxy for the
/// federation-only Map/Document panels (and inclusively surfaces peer-fed
/// charts/tables); native chart/table streams are counted directly.
async fn workspace_panels(pool: &sqlx::PgPool, workspace_id: Uuid) -> Result<Value, AppError> {
    let charts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1 AND s.view_config IS NOT NULL",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1 AND jsonb_exists(s.view_config, 'property_fields')",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    // Event layers are native point streams: view_config carries both a
    // longitude and latitude pointer. Drives the Map tab for no-peer
    // artifact-path workspaces (onramp ONR-004).
    let event_layers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM streams s
         JOIN connectors c ON c.id = s.connector_id
         WHERE c.workspace_id = $1
           AND jsonb_exists(s.view_config, 'lon_pointer')
           AND jsonb_exists(s.view_config, 'lat_pointer')",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let has_active_peer: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM workspace_peer_bindings b
            JOIN peers p ON p.id = b.peer_id
            WHERE b.workspace_id = $1 AND b.status = 'active' AND p.status = 'active'
         )",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let approvals_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1 AND ap.status = 'pending'",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(json!({
        "charts": charts,
        "tables": tables,
        "eventLayers": event_layers,
        "hasActivePeer": has_active_peer,
        "approvalsPending": approvals_pending,
    }))
}

pub async fn close_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, id, ctx.org_id).await?;
    let repo = WorkspaceRepo::new(state.pool.clone());
    // Verify workspace exists first
    repo.get(id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("workspace {} not found", id)))?;

    let ws = repo.close(id).await.map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::to_value(ws).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn list_roles(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let repo = RoleRepo::new(state.pool.clone());
    let items = repo.list(workspace_id).await.map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn list_peer_tools(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let manifest =
        crate::services::federation::workspace_peer_manifest(&state, workspace_id, peer_id, &ctx)
            .await
            .map_err(AppError::Internal)?;
    let peer = PeerRepo::new(state.pool.clone())
        .get_for_org(peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer not found".into()))?;
    let tools = crate::services::federation::namespaced_tools_from_manifest(&peer, &manifest);
    Ok(Json(json!({
        "peerId": peer_id,
        "stale": manifest.stale,
        "fetchedAt": manifest.fetched_at,
        "items": tools,
    })))
}

pub async fn list_peer_resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let body =
        crate::services::federation::workspace_peer_resources(&state, workspace_id, peer_id, &ctx)
            .await
            .map_err(AppError::Internal)?;
    Ok(Json(body))
}

pub async fn list_context_slices(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let items = crate::services::federation::workspace_context_slices(&state, workspace_id, &ctx)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
