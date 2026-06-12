use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    Extension,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, permission_grants, require_permission, AuthContext},
    error::AppError,
    models::{ActorKind, AutoExecPolicy},
    repos::{AuditEventRepo, AutoExecPolicyInput, AutoExecPolicyRepo, ConnectorRepo, RoleRepo},
    routes::roles::is_valid_workspace_permission,
    services::auto_exec::MAX_RATE_LIMIT_PER_MIN,
    state::AppState,
};

#[derive(Deserialize, Default)]
pub struct TriggerBody {
    pub signal_title_prefix: Option<String>,
    pub severity_at_most: Option<String>,
}

#[derive(Deserialize)]
pub struct PolicyBody {
    pub name: String,
    #[serde(default)]
    pub trigger: TriggerBody,
    pub connector_id: Uuid,
    pub op: String,
    #[serde(default = "default_args_template")]
    pub args_template: Value,
    pub rate_limit_per_min: i64,
    /// Omitted → 'routine', the safe floor (design AC-8).
    pub severity_cap: Option<String>,
    pub authorized_by_permission: String,
    /// Omitted → true.
    pub enabled: Option<bool>,
}

fn default_args_template() -> Value {
    json!({})
}

fn validation_error(message: String) -> AppError {
    AppError::UnprocessableEntityJson(json!({
        "error": "validation",
        "message": message,
    }))
}

/// API shape per the design contract: nested `trigger`, no `org_id`.
fn policy_json(p: &AutoExecPolicy) -> Value {
    json!({
        "id": p.id,
        "workspace_id": p.workspace_id,
        "name": p.name,
        "trigger": {
            "signal_title_prefix": p.trigger_signal_title_prefix,
            "severity_at_most": p.trigger_severity_at_most,
        },
        "connector_id": p.connector_id,
        "op": p.op,
        "args_template": p.args_template,
        "rate_limit_per_min": p.rate_limit_per_min,
        "severity_cap": p.severity_cap,
        "authorized_by_permission": p.authorized_by_permission,
        "enabled": p.enabled,
        "created_by": p.created_by,
        "created_at": p.created_at,
        "updated_at": p.updated_at,
    })
}

/// Fail-closed write-time validation (422), then Guards A and B.
/// Returns the validated repo input.
async fn validate_body(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
    body: PolicyBody,
) -> Result<AutoExecPolicyInput, AppError> {
    if body.name.trim().is_empty() {
        return Err(validation_error("name must be non-empty".into()));
    }

    let severity_cap = body.severity_cap.unwrap_or_else(|| "routine".to_owned());
    if !matches!(severity_cap.as_str(), "routine" | "flagged") {
        return Err(validation_error(format!(
            "severity_cap '{severity_cap}' is not allowed; must be 'routine' or 'flagged'"
        )));
    }
    if let Some(at_most) = &body.trigger.severity_at_most {
        if !matches!(at_most.as_str(), "routine" | "flagged") {
            return Err(validation_error(format!(
                "trigger.severity_at_most '{at_most}' is not allowed; must be 'routine' or 'flagged'"
            )));
        }
    }
    if body.rate_limit_per_min < 1 || body.rate_limit_per_min > MAX_RATE_LIMIT_PER_MIN as i64 {
        return Err(validation_error(format!(
            "rate_limit_per_min must be between 1 and {MAX_RATE_LIMIT_PER_MIN}"
        )));
    }
    if !is_valid_workspace_permission(&body.authorized_by_permission) {
        return Err(validation_error(format!(
            "authorized_by_permission '{}' is not in the workspace permission vocabulary",
            body.authorized_by_permission
        )));
    }

    // Guard A — authorship escalation: the creator must hold the permission the
    // policy executes under; `admin` holders are exempt.
    let (held, _) = RoleRepo::new(state.pool.clone())
        .effective_permissions(ctx.user_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    if !held.contains("admin") && !permission_grants(&held, &body.authorized_by_permission) {
        return Err(AppError::ConflictJson(json!({
            "error": "permission_escalation",
            "message": format!(
                "cannot author a policy under '{}': you do not hold it in this workspace",
                body.authorized_by_permission
            ),
        })));
    }

    // Guard B — connector workspace scope: data-tenancy constraint, `admin`
    // is NOT exempt.
    let connector = ConnectorRepo::new(state.pool.clone())
        .get_for_workspace(body.connector_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    if connector.is_none() {
        return Err(validation_error(format!(
            "connector {} does not exist in this workspace",
            body.connector_id
        )));
    }

    Ok(AutoExecPolicyInput {
        name: body.name,
        trigger_signal_title_prefix: body.trigger.signal_title_prefix,
        trigger_severity_at_most: body.trigger.severity_at_most,
        connector_id: body.connector_id,
        op: body.op,
        args_template: body.args_template,
        rate_limit_per_min: body.rate_limit_per_min as i32,
        severity_cap,
        authorized_by_permission: body.authorized_by_permission,
        enabled: body.enabled.unwrap_or(true),
    })
}

fn map_name_conflict(e: anyhow::Error, name: &str) -> AppError {
    let is_unique = e
        .downcast_ref::<sqlx::Error>()
        .and_then(|se| se.as_database_error())
        .and_then(|db| db.code())
        .is_some_and(|code| code == "23505");
    if is_unique {
        validation_error(format!(
            "a policy named '{name}' already exists in this workspace"
        ))
    } else {
        AppError::Internal(e)
    }
}

async fn audit_policy_write(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
    verb: &str,
    policy_id: Uuid,
    payload: Value,
) -> Result<(), AppError> {
    AuditEventRepo::new(state.pool.clone())
        .insert(
            Some(workspace_id),
            ActorKind::User,
            &ctx.user_id.to_string(),
            verb,
            "auto_exec_policy",
            Some(policy_id),
            payload,
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(())
}

/// GET /api/v1/workspaces/:id/auto-exec-policies
pub async fn list_policies(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "approvals:decide").await?;
    let items = AutoExecPolicyRepo::new(state.pool.clone())
        .list_for_workspace(workspace_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({
        "items": items.iter().map(policy_json).collect::<Vec<_>>()
    })))
}

/// POST /api/v1/workspaces/:id/auto-exec-policies
pub async fn create_policy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PolicyBody>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "approvals:decide").await?;

    let name = body.name.clone();
    let input = validate_body(&state, &ctx, workspace_id, body).await?;
    let created = AutoExecPolicyRepo::new(state.pool.clone())
        .create(workspace_id, ctx.user_id, &input)
        .await
        .map_err(|e| map_name_conflict(e, &name))?;

    audit_policy_write(
        &state,
        &ctx,
        workspace_id,
        "auto_exec_policy.created",
        created.id,
        json!({ "actor": ctx.user_id, "after": policy_json(&created) }),
    )
    .await?;

    Ok(Json(policy_json(&created)))
}

/// PUT /api/v1/workspaces/:id/auto-exec-policies/:policyId (full replace)
pub async fn update_policy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, policy_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PolicyBody>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "approvals:decide").await?;

    let repo = AutoExecPolicyRepo::new(state.pool.clone());
    let before = repo
        .get(policy_id, workspace_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("policy not found".into()))?;

    let name = body.name.clone();
    let input = validate_body(&state, &ctx, workspace_id, body).await?;
    let updated = repo
        .update(policy_id, workspace_id, &input)
        .await
        .map_err(|e| map_name_conflict(e, &name))?
        .ok_or_else(|| AppError::NotFound("policy not found".into()))?;

    audit_policy_write(
        &state,
        &ctx,
        workspace_id,
        "auto_exec_policy.updated",
        updated.id,
        json!({
            "actor": ctx.user_id,
            "before": policy_json(&before),
            "after": policy_json(&updated),
        }),
    )
    .await?;

    Ok(Json(policy_json(&updated)))
}

/// DELETE /api/v1/workspaces/:id/auto-exec-policies/:policyId
pub async fn delete_policy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, policy_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "approvals:decide").await?;

    let repo = AutoExecPolicyRepo::new(state.pool.clone());
    let before = repo
        .get(policy_id, workspace_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("policy not found".into()))?;

    let removed = repo
        .delete(policy_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    if !removed {
        return Err(AppError::NotFound("policy not found".into()));
    }

    audit_policy_write(
        &state,
        &ctx,
        workspace_id,
        "auto_exec_policy.deleted",
        policy_id,
        json!({ "actor": ctx.user_id, "before": policy_json(&before) }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
