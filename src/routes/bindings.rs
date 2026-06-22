use axum::{
    extract::{Path, State},
    response::Json,
    Extension,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    repos::WorkspacePeerBindingRepo,
    services::workspace_peer_binding::{self, RefreshError},
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBindingRequest {
    pub peer_id: Uuid,
    pub foreign_tenant_id: String,
    pub foreign_workspace_id: Option<String>,
    #[serde(default = "empty_object")]
    pub scope: Value,
}

fn empty_object() -> Value {
    json!({})
}

pub async fn list_for_workspace(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let items = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_by_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn list_for_peer(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let items = WorkspacePeerBindingRepo::new(state.pool.clone())
        .list_by_peer(peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

pub async fn get_binding(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, binding_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let binding = WorkspacePeerBindingRepo::new(state.pool.clone())
        .get_by_id_org_scoped(binding_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("workspace binding not found".into()))?;
    if binding.workspace_id != workspace_id {
        return Err(AppError::NotFound("workspace binding not found".into()));
    }
    Ok(Json(
        serde_json::to_value(binding).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn create_binding(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateBindingRequest>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let tenant_id = validate_tenant_id(&req.foreign_tenant_id)?;
    validate_scope(&req.scope)?;
    let foreign_workspace_id = req
        .foreign_workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let repo = WorkspacePeerBindingRepo::new(state.pool.clone());
    let binding = repo
        .create_manual(
            workspace_id,
            req.peer_id,
            ctx.org_id,
            &tenant_id,
            foreign_workspace_id,
            req.scope,
        )
        .await
        .map_err(map_create_error)?;
    Ok(Json(
        serde_json::to_value(binding).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn patch_binding(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, binding_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    ensure_binding_in_workspace(&state, ctx.org_id, workspace_id, binding_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let foreign_tenant_id = match req.get("foreignTenantId") {
        Some(v) => Some(validate_tenant_id(v.as_str().ok_or_else(|| {
            AppError::UnprocessableEntity("foreignTenantId must be a string".into())
        })?)?),
        None => None,
    };
    let foreign_workspace_id = if req.get("foreignWorkspaceId").is_some() {
        let value = req.get("foreignWorkspaceId").unwrap_or(&Value::Null);
        let next = if value.is_null() {
            None
        } else {
            Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        AppError::UnprocessableEntity(
                            "foreignWorkspaceId must be a string or null".into(),
                        )
                    })?
                    .trim(),
            )
            .filter(|s| !s.is_empty())
        };
        Some(next)
    } else {
        None
    };
    let scope = match req.get("scope") {
        Some(v) => {
            validate_scope(v)?;
            Some(v.clone())
        }
        None => None,
    };

    let binding = WorkspacePeerBindingRepo::new(state.pool.clone())
        .patch(
            binding_id,
            ctx.org_id,
            foreign_tenant_id.as_deref(),
            foreign_workspace_id,
            scope,
        )
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("workspace binding not found".into()))?;
    Ok(Json(
        serde_json::to_value(binding).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

pub async fn delete_binding(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, binding_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_binding_in_workspace(&state, ctx.org_id, workspace_id, binding_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let deleted = WorkspacePeerBindingRepo::new(state.pool.clone())
        .delete_by_id_org_scoped(binding_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    if !deleted {
        return Err(AppError::NotFound("workspace binding not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn refresh_binding(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, binding_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_binding_in_workspace(&state, ctx.org_id, workspace_id, binding_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    match workspace_peer_binding::refresh_binding(&state, binding_id, ctx.org_id).await {
        Ok(binding) => Ok(Json(
            serde_json::to_value(binding).map_err(|e| AppError::Internal(e.into()))?,
        )),
        Err(RefreshError::NotFound | RefreshError::PeerGone) => {
            Err(AppError::NotFound("workspace binding not found".into()))
        }
        Err(RefreshError::Unreachable(message)) => {
            let peer_id = WorkspacePeerBindingRepo::new(state.pool.clone())
                .peer_for_binding(binding_id, ctx.org_id)
                .await
                .map_err(AppError::Internal)?
                .unwrap_or(Uuid::nil());
            Err(AppError::WhoamiUnreachable { peer_id, message })
        }
        Err(RefreshError::Conflict { old, new }) => {
            Err(AppError::WorkspaceBindingConflict { old, new })
        }
        Err(RefreshError::Db(e)) => Err(AppError::Internal(e)),
    }
}

async fn ensure_binding_in_workspace(
    state: &AppState,
    org_id: Uuid,
    workspace_id: Uuid,
    binding_id: Uuid,
) -> Result<(), AppError> {
    let binding = WorkspacePeerBindingRepo::new(state.pool.clone())
        .get_by_id_org_scoped(binding_id, org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("workspace binding not found".into()))?;
    if binding.workspace_id != workspace_id {
        return Err(AppError::NotFound("workspace binding not found".into()));
    }
    Ok(())
}

fn validate_tenant_id(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::UnprocessableEntity(
            "foreignTenantId cannot be empty or whitespace".into(),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_scope(scope: &Value) -> Result<(), AppError> {
    if !scope.is_object() {
        return Err(AppError::UnprocessableEntity(
            "scope must be a JSON object".into(),
        ));
    }
    Ok(())
}

fn map_create_error(e: anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("binding target not found") {
        AppError::NotFound("workspace or peer not found".into())
    } else if msg.contains("duplicate")
        || msg.contains("unique")
        || msg.contains("wpb_unique_workspace_peer")
    {
        AppError::WorkspaceBindingConflict {
            old: "existing".into(),
            new: "duplicate".into(),
        }
    } else {
        AppError::Internal(e)
    }
}
