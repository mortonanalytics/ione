//! Declarative workspace-graph provisioning (design Slice 3).
//!
//! `apply` runs the whole spec in one transaction under an org advisory lock so
//! concurrent re-applies of the same org's spec serialize. Each entity is
//! read-then-written within the lock: missing → created, present-and-different →
//! updated (with changed fields), present-and-identical → unchanged. Any error
//! rolls the whole transaction back; nothing persists.
//!
//! Merge semantics: resources not in the spec are never deleted. Entities match
//! on their natural keys — workspace on `(org_id, name)`, role/connector on
//! `(workspace_id, name)`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    error::AppError,
    models::{ActorKind, ConnectorKind, WorkspaceLifecycle},
    state::AppState,
};

// ─── Spec ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionSpec {
    pub version: String,
    pub workspace: WorkspaceSpec,
    #[serde(default)]
    pub roles: Vec<RoleSpec>,
    #[serde(default)]
    pub connectors: Vec<ConnectorSpec>,
    // Accepted in the schema for forward-compatibility; provisioning these is a
    // named fast-follow (peers need trust-issuer resolution; auto_exec_policies
    // require a NOT NULL created_by users(id) a service account cannot satisfy).
    // A non-empty section is rejected rather than silently ignored.
    #[serde(default)]
    pub peers: Vec<Value>,
    #[serde(default)]
    pub bindings: Vec<Value>,
    #[serde(default)]
    pub auto_exec_policies: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSpec {
    pub name: String,
    pub domain: Option<String>,
    pub lifecycle: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleSpec {
    pub name: String,
    pub coc_level: i32,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorSpec {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub config: Value,
}

// ─── Result ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedEntity {
    pub kind: String,
    pub id: Uuid,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedEntity {
    pub kind: String,
    pub id: Uuid,
    pub name: String,
    pub changed_fields: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionResult {
    pub workspace_id: Uuid,
    pub created: Vec<CreatedEntity>,
    pub updated: Vec<UpdatedEntity>,
    pub unchanged_count: i64,
}

fn escalation_error(detail: String) -> AppError {
    AppError::ConflictJson(json!({ "error": "permission_escalation", "message": detail }))
}

fn entity_error(entity: &str, name: &str, detail: &str) -> AppError {
    AppError::UnprocessableEntityJson(json!({
        "error": "invalid_entity",
        "entity": entity,
        "name": name,
        "message": detail,
    }))
}

fn parse_lifecycle(s: &str) -> Result<WorkspaceLifecycle, AppError> {
    match s {
        "continuous" => Ok(WorkspaceLifecycle::Continuous),
        "bounded" => Ok(WorkspaceLifecycle::Bounded),
        other => Err(AppError::BadRequest(format!("invalid lifecycle '{other}'"))),
    }
}

fn parse_connector_kind(s: &str) -> Option<ConnectorKind> {
    match s {
        "mcp" => Some(ConnectorKind::Mcp),
        "openapi" => Some(ConnectorKind::Openapi),
        "rust_native" => Some(ConnectorKind::RustNative),
        _ => None,
    }
}

/// Apply a provision spec. See module docs for the transactional contract.
pub async fn apply(
    state: &AppState,
    ctx: &AuthContext,
    spec: ProvisionSpec,
) -> Result<ProvisionResult, AppError> {
    if spec.version != "v1" {
        return Err(AppError::BadRequest(format!(
            "unsupported spec version '{}'; expected 'v1'",
            spec.version
        )));
    }
    if !spec.peers.is_empty() || !spec.bindings.is_empty() || !spec.auto_exec_policies.is_empty() {
        return Err(AppError::UnprocessableEntityJson(json!({
            "error": "unsupported_spec_section",
            "message": "peers, bindings, and auto_exec_policies provisioning is not yet supported; \
                        provision them via their own endpoints",
        })));
    }

    let actor: HashSet<String> = ctx.permissions.iter().cloned().collect();
    let actor_max_coc = ctx.service_account_token_id.map(|_| ()); // marker; real cap below
    let _ = actor_max_coc;

    let mut created = Vec::new();
    let mut updated = Vec::new();
    let mut unchanged_count: i64 = 0;

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // Serialize concurrent re-applies of the same org's spec.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ione_provision'), hashtext($1))")
        .bind(ctx.org_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // The actor's coc ceiling: a service-account token's provisionable_max_coc.
    let max_coc = actor_coc_ceiling(&mut tx, ctx).await?;

    // 1. Workspace (org_id, name).
    let workspace_id = upsert_workspace(
        &mut tx,
        ctx.org_id,
        &spec.workspace,
        &mut created,
        &mut updated,
        &mut unchanged_count,
    )
    .await?;

    // 2. Roles (workspace_id, name) — escalation guard per role.
    for role in &spec.roles {
        for p in &role.permissions {
            if !crate::auth::permission_grants(&actor, p) {
                return Err(escalation_error(format!(
                    "role '{}' grants '{p}' which the provisioning actor does not hold",
                    role.name
                )));
            }
        }
        if role.coc_level > max_coc {
            return Err(escalation_error(format!(
                "role '{}' coc_level {} exceeds the actor's provisionable_max_coc {}",
                role.name, role.coc_level, max_coc
            )));
        }
        upsert_role(
            &mut tx,
            workspace_id,
            role,
            &mut created,
            &mut updated,
            &mut unchanged_count,
        )
        .await?;
    }

    // 3. Creator-membership: the actor gets a membership in the workspace on a
    //    synthesized role capped at its own permissions, so it can manage what
    //    it provisioned. Skipped for a nil principal (no users row to bind).
    if !ctx.user_id.is_nil() {
        grant_creator_membership(&mut tx, workspace_id, ctx, max_coc).await?;
    }

    // 4. Connectors (workspace_id, name).
    for connector in &spec.connectors {
        let kind = parse_connector_kind(&connector.kind).ok_or_else(|| {
            entity_error(
                "connector",
                &connector.name,
                &format!("invalid connector kind '{}'", connector.kind),
            )
        })?;
        upsert_connector(
            &mut tx,
            workspace_id,
            connector,
            kind,
            &mut created,
            &mut updated,
            &mut unchanged_count,
        )
        .await?;
    }

    // Audit (one summary row; connector configs are never written).
    let actor_ref = match ctx.service_account_token_id {
        Some(id) => id.to_string(),
        None => ctx.user_id.to_string(),
    };
    let actor_kind = if ctx.is_service_account {
        ActorKind::ServiceAccount
    } else {
        ActorKind::User
    };
    let created_summary: Vec<Value> = created
        .iter()
        .map(|c| json!({ "kind": c.kind, "id": c.id, "name": c.name }))
        .collect();
    let updated_summary: Vec<Value> = updated
        .iter()
        .map(|u| json!({ "kind": u.kind, "id": u.id, "name": u.name, "changedFields": u.changed_fields }))
        .collect();
    // Org-level event: audit_events has no org_id column, so workspace_id is
    // NULL and org_id rides in the payload (per the audit-export contract).
    sqlx::query(
        "INSERT INTO audit_events
           (workspace_id, actor_kind, actor_ref, verb, object_kind, object_id, payload)
         VALUES (NULL, $1, $2, 'provisioning.applied', 'org', NULL, $3)",
    )
    .bind(actor_kind)
    .bind(&actor_ref)
    .bind(json!({
        "org_id": ctx.org_id,
        "workspace_id": workspace_id,
        "spec_name": spec.workspace.name,
        "created": created_summary,
        "updated": updated_summary,
        "unchanged_count": unchanged_count,
    }))
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    tx.commit().await.map_err(|e| AppError::Internal(e.into()))?;

    Ok(ProvisionResult {
        workspace_id,
        created,
        updated,
        unchanged_count,
    })
}

/// The actor's coc ceiling. For a service-account token it is the token's
/// `provisionable_max_coc`; for a session actor (rare via this endpoint) it is
/// their effective max coc across the org.
async fn actor_coc_ceiling(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &AuthContext,
) -> Result<i32, AppError> {
    if let Some(token_id) = ctx.service_account_token_id {
        let coc: i32 = sqlx::query_scalar(
            "SELECT provisionable_max_coc FROM service_account_tokens WHERE id = $1",
        )
        .bind(token_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .unwrap_or(0);
        return Ok(coc);
    }
    let coc: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(r.coc_level), 0)
         FROM memberships m
              JOIN roles r ON r.id = m.role_id
              JOIN workspaces w ON w.id = m.workspace_id
         WHERE m.user_id = $1 AND w.org_id = $2",
    )
    .bind(ctx.user_id)
    .bind(ctx.org_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(coc)
}

async fn upsert_workspace(
    tx: &mut Transaction<'_, Postgres>,
    org_id: Uuid,
    spec: &WorkspaceSpec,
    created: &mut Vec<CreatedEntity>,
    updated: &mut Vec<UpdatedEntity>,
    unchanged_count: &mut i64,
) -> Result<Uuid, AppError> {
    let domain = spec.domain.as_deref().unwrap_or("generic");
    let lifecycle = parse_lifecycle(spec.lifecycle.as_deref().unwrap_or("continuous"))?;
    let metadata = spec.metadata.clone().unwrap_or_else(|| json!({}));

    let existing: Option<(Uuid, String, WorkspaceLifecycle, Value)> = sqlx::query_as(
        "SELECT id, domain, lifecycle, metadata FROM workspaces WHERE org_id = $1 AND name = $2",
    )
    .bind(org_id)
    .bind(&spec.name)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    match existing {
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO workspaces (org_id, name, domain, lifecycle, metadata)
                 VALUES ($1, $2, $3, $4, $5) RETURNING id",
            )
            .bind(org_id)
            .bind(&spec.name)
            .bind(domain)
            .bind(&lifecycle)
            .bind(&metadata)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            created.push(CreatedEntity {
                kind: "workspace".into(),
                id,
                name: spec.name.clone(),
            });
            Ok(id)
        }
        Some((id, cur_domain, cur_lifecycle, cur_metadata)) => {
            let mut changed = Vec::new();
            if cur_domain != domain {
                changed.push("domain".to_string());
            }
            if cur_lifecycle != lifecycle {
                changed.push("lifecycle".to_string());
            }
            if cur_metadata != metadata {
                changed.push("metadata".to_string());
            }
            if changed.is_empty() {
                *unchanged_count += 1;
            } else {
                sqlx::query(
                    "UPDATE workspaces SET domain = $2, lifecycle = $3, metadata = $4 WHERE id = $1",
                )
                .bind(id)
                .bind(domain)
                .bind(&lifecycle)
                .bind(&metadata)
                .execute(&mut **tx)
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
                updated.push(UpdatedEntity {
                    kind: "workspace".into(),
                    id,
                    name: spec.name.clone(),
                    changed_fields: changed,
                });
            }
            Ok(id)
        }
    }
}

async fn upsert_role(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    spec: &RoleSpec,
    created: &mut Vec<CreatedEntity>,
    updated: &mut Vec<UpdatedEntity>,
    unchanged_count: &mut i64,
) -> Result<(), AppError> {
    let perms = json!(spec.permissions);
    let existing: Option<(Uuid, i32, Value)> = sqlx::query_as(
        "SELECT id, coc_level, permissions FROM roles WHERE workspace_id = $1 AND name = $2",
    )
    .bind(workspace_id)
    .bind(&spec.name)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    match existing {
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO roles (workspace_id, name, coc_level, permissions)
                 VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(workspace_id)
            .bind(&spec.name)
            .bind(spec.coc_level)
            .bind(&perms)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            created.push(CreatedEntity {
                kind: "role".into(),
                id,
                name: spec.name.clone(),
            });
        }
        Some((id, cur_coc, cur_perms)) => {
            let mut changed = Vec::new();
            if cur_coc != spec.coc_level {
                changed.push("coc_level".to_string());
            }
            if !json_array_eq(&cur_perms, &perms) {
                changed.push("permissions".to_string());
            }
            if changed.is_empty() {
                *unchanged_count += 1;
            } else {
                sqlx::query("UPDATE roles SET coc_level = $2, permissions = $3 WHERE id = $1")
                    .bind(id)
                    .bind(spec.coc_level)
                    .bind(&perms)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| AppError::Internal(e.into()))?;
                updated.push(UpdatedEntity {
                    kind: "role".into(),
                    id,
                    name: spec.name.clone(),
                    changed_fields: changed,
                });
            }
        }
    }
    Ok(())
}

async fn upsert_connector(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    spec: &ConnectorSpec,
    kind: ConnectorKind,
    created: &mut Vec<CreatedEntity>,
    updated: &mut Vec<UpdatedEntity>,
    unchanged_count: &mut i64,
) -> Result<(), AppError> {
    let existing: Option<(Uuid, ConnectorKind, Value)> = sqlx::query_as(
        "SELECT id, kind, config FROM connectors WHERE workspace_id = $1 AND name = $2",
    )
    .bind(workspace_id)
    .bind(&spec.name)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    match existing {
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO connectors (workspace_id, kind, name, config)
                 VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(workspace_id)
            .bind(&kind)
            .bind(&spec.name)
            .bind(&spec.config)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            created.push(CreatedEntity {
                kind: "connector".into(),
                id,
                name: spec.name.clone(),
            });
        }
        Some((id, cur_kind, cur_config)) => {
            let mut changed = Vec::new();
            if cur_kind != kind {
                changed.push("kind".to_string());
            }
            if cur_config != spec.config {
                changed.push("config".to_string());
            }
            if changed.is_empty() {
                *unchanged_count += 1;
            } else {
                sqlx::query("UPDATE connectors SET kind = $2, config = $3 WHERE id = $1")
                    .bind(id)
                    .bind(&kind)
                    .bind(&spec.config)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| AppError::Internal(e.into()))?;
                updated.push(UpdatedEntity {
                    kind: "connector".into(),
                    id,
                    name: spec.name.clone(),
                    changed_fields: changed,
                });
            }
        }
    }
    Ok(())
}

/// Grant the provisioning actor a membership in the workspace on a synthesized
/// `provisioner` role capped at the actor's own permissions and coc ceiling.
/// Idempotent: re-applies reuse the role and skip a duplicate membership.
async fn grant_creator_membership(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    ctx: &AuthContext,
    max_coc: i32,
) -> Result<(), AppError> {
    let perms = json!(ctx.permissions);
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'provisioner', $2, $3)
         ON CONFLICT (workspace_id, name) DO UPDATE
           SET coc_level = EXCLUDED.coc_level, permissions = EXCLUDED.permissions
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(max_coc)
    .bind(&perms)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query(
        "INSERT INTO memberships (user_id, workspace_id, role_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id, workspace_id, role_id) DO NOTHING",
    )
    .bind(ctx.user_id)
    .bind(workspace_id)
    .bind(role_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Order-insensitive equality of two JSON string arrays.
fn json_array_eq(a: &Value, b: &Value) -> bool {
    let to_set = |v: &Value| -> Option<HashSet<String>> {
        match v {
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(String::from))
                    .collect(),
            ),
            _ => None,
        }
    };
    match (to_set(a), to_set(b)) {
        (Some(sa), Some(sb)) => sa == sb,
        _ => a == b,
    }
}
