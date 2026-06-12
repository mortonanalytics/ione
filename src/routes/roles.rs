use std::collections::HashSet;

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
    models::ActorKind,
    repos::{AuditEventRepo, MembershipRepo, RoleRepo},
    state::AppState,
};

/// The closed workspace-scoped vocabulary. `tool_invoke:<peer>:<tool>` scopes
/// are validated structurally. Org-scoped strings (`trust_issuers:manage`)
/// are NOT assignable through the role editor.
const WORKSPACE_VOCABULARY: &[&str] = &[
    "admin",
    "audit:read",
    "roles:manage",
    "peers:manage",
    "approvals:decide",
    "workspace:write",
];

fn is_valid_workspace_permission(s: &str) -> bool {
    if WORKSPACE_VOCABULARY.contains(&s) {
        return true;
    }
    let segs: Vec<&str> = s.split(':').collect();
    segs.len() == 3 && segs[0] == "tool_invoke" && segs.iter().all(|seg| !seg.is_empty())
}

fn permissions_set(value: &Value) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Value::Array(items) = value {
        for item in items {
            if let Value::String(s) = item {
                set.insert(s.clone());
            }
        }
    }
    set
}

fn escalation_error(detail: String) -> AppError {
    AppError::ConflictJson(json!({
        "error": "permission_escalation",
        "message": detail,
    }))
}

/// Escalation guard shared by the permission editor and membership grants:
/// a non-`admin` actor may only hand out permissions they hold and may not
/// touch coc levels above their own effective max.
struct ActorAuthority {
    held: HashSet<String>,
    max_coc: i32,
    is_admin: bool,
}

async fn actor_authority(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
) -> Result<ActorAuthority, AppError> {
    let (held, max_coc) = RoleRepo::new(state.pool.clone())
        .effective_permissions(ctx.user_id, workspace_id)
        .await
        .map_err(AppError::Internal)?;
    let is_admin = held.contains("admin");
    Ok(ActorAuthority {
        held,
        max_coc,
        is_admin,
    })
}

/// GET /api/v1/workspaces/:id/roles
pub async fn list_roles_detailed(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "roles:manage").await?;
    let items = RoleRepo::new(state.pool.clone())
        .list_with_member_count(workspace_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutPermissionsBody {
    pub permissions: Vec<String>,
    pub coc_level: Option<i32>,
}

/// PUT /api/v1/workspaces/:id/roles/:roleId/permissions
pub async fn put_role_permissions(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, role_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PutPermissionsBody>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "roles:manage").await?;

    for p in &body.permissions {
        if !is_valid_workspace_permission(p) {
            return Err(AppError::BadRequestJson(json!({
                "error": "invalid_permission",
                "message": format!("'{p}' is not in the workspace permission vocabulary"),
            })));
        }
    }

    let repo = RoleRepo::new(state.pool.clone());
    let current = repo
        .get_in_workspace(role_id, workspace_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("role not found".into()))?;

    let actor = actor_authority(&state, &ctx, workspace_id).await?;
    if !actor.is_admin {
        let before = permissions_set(&current.permissions);
        for p in &body.permissions {
            if !before.contains(p) && !permission_grants(&actor.held, p) {
                return Err(escalation_error(format!(
                    "cannot grant '{p}': you do not hold it in this workspace"
                )));
            }
        }
        if let Some(coc) = body.coc_level {
            if coc > current.coc_level && coc > actor.max_coc {
                return Err(escalation_error(format!(
                    "cannot raise coc_level to {coc}: above your effective max {}",
                    actor.max_coc
                )));
            }
        }
    }

    let new_permissions = json!(body.permissions);
    let updated = repo
        .set_permissions(role_id, workspace_id, &new_permissions, body.coc_level)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("role not found".into()))?;

    AuditEventRepo::new(state.pool.clone())
        .insert(
            Some(workspace_id),
            ActorKind::User,
            &ctx.user_id.to_string(),
            "role.permissions.updated",
            "role",
            Some(role_id),
            json!({
                "actor": ctx.user_id,
                "before": current.permissions,
                "after": updated.permissions,
                "cocBefore": current.coc_level,
                "cocAfter": updated.coc_level,
            }),
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(
        serde_json::to_value(&updated).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

#[derive(Deserialize)]
pub struct PostMembershipBody {
    pub user_id: Uuid,
    pub role_id: Uuid,
}

/// POST /api/v1/workspaces/:id/memberships
pub async fn post_membership(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PostMembershipBody>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "roles:manage").await?;

    let role = RoleRepo::new(state.pool.clone())
        .get_in_workspace(body.role_id, workspace_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("role not found".into()))?;
    let user_in_org: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND org_id = $2)")
            .bind(body.user_id)
            .bind(ctx.org_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    if !user_in_org {
        return Err(AppError::NotFound("user not found".into()));
    }

    // Granting a membership hands out the role's whole permission set, so the
    // same escalation guard applies as when editing that set.
    let actor = actor_authority(&state, &ctx, workspace_id).await?;
    if !actor.is_admin {
        for p in permissions_set(&role.permissions) {
            if !permission_grants(&actor.held, &p) {
                return Err(escalation_error(format!(
                    "cannot grant membership in '{}': it holds '{p}' which you do not",
                    role.name
                )));
            }
        }
        if role.coc_level > actor.max_coc {
            return Err(escalation_error(format!(
                "cannot grant membership in '{}': its coc_level {} is above your effective max {}",
                role.name, role.coc_level, actor.max_coc
            )));
        }
    }

    let membership_id = MembershipRepo::new(state.pool.clone())
        .grant(body.user_id, workspace_id, body.role_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::ConflictJson(json!({
                "error": "membership_exists",
                "message": "the user already holds this role in this workspace",
            }))
        })?;

    AuditEventRepo::new(state.pool.clone())
        .insert(
            Some(workspace_id),
            ActorKind::User,
            &ctx.user_id.to_string(),
            "membership.granted",
            "membership",
            Some(membership_id),
            json!({
                "actor": ctx.user_id,
                "userId": body.user_id,
                "roleId": body.role_id,
            }),
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({ "id": membership_id })))
}

/// DELETE /api/v1/workspaces/:id/memberships/:userId/:roleId
pub async fn delete_membership(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, user_id, role_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "roles:manage").await?;

    let removed = MembershipRepo::new(state.pool.clone())
        .revoke(user_id, workspace_id, role_id)
        .await
        .map_err(AppError::Internal)?;
    if !removed {
        return Err(AppError::NotFound("membership not found".into()));
    }

    AuditEventRepo::new(state.pool.clone())
        .insert(
            Some(workspace_id),
            ActorKind::User,
            &ctx.user_id.to_string(),
            "membership.revoked",
            "membership",
            None,
            json!({
                "actor": ctx.user_id,
                "userId": user_id,
                "roleId": role_id,
            }),
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}
