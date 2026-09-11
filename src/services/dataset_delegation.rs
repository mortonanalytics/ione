use crate::{
    auth::{ensure_workspace_in_org, AuthContext},
    error::AppError,
    models::dataset_delegation::{DatasetDelegation, DatasetIdentity},
    repos::RoleRepo,
    routes::relay,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Extension,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub async fn owner_identity(
    state: &AppState,
    ctx: &AuthContext,
    workspace: Uuid,
) -> Result<DatasetIdentity, AppError> {
    ensure_workspace_in_org(&state.pool, workspace, ctx.org_id).await?;
    if ctx.is_service_account || ctx.user_id.is_nil() {
        return Err(AppError::Forbidden);
    }
    let member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM org_memberships WHERE user_id=$1 AND org_id=$2)",
    )
    .bind(ctx.user_id)
    .bind(ctx.org_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let (permissions, _) = RoleRepo::new(state.pool.clone())
        .effective_permissions(ctx.user_id, workspace)
        .await
        .map_err(AppError::Internal)?;
    if !member
        || !permissions.contains("data:datasets:write")
        || !permissions.contains("data:query")
    {
        return Err(AppError::Forbidden);
    }
    Ok(DatasetIdentity {
        deployment_id: state
            .config
            .relay
            .as_ref()
            .ok_or(AppError::Forbidden)?
            .deployment_id,
        tenant_id: ctx.org_id,
        workspace_id: workspace,
        actor_id: ctx.user_id.to_string(),
        service_account_id: format!("ione:{}", ctx.org_id),
    })
}

pub async fn manifest(
    state: &AppState,
    ctx: &AuthContext,
    workspace: Uuid,
    dataset: Uuid,
    version: Uuid,
) -> Result<Value, AppError> {
    owner_identity(state, ctx, workspace).await?;
    let axum::Json(value) = relay::dataset_version(
        State(state.clone()),
        Extension(ctx.clone()),
        Path((workspace, dataset, version)),
    )
    .await?;
    owner_identity(state, ctx, workspace).await?;
    const FIELDS: &[&str] = &[
        "deployment_id",
        "tenant_id",
        "workspace_id",
        "dataset_id",
        "version_id",
        "destination_id",
        "dataset_name",
        "run_id",
        "requires_source_access",
        "allows_delegation",
        "ttl_seconds",
        "owner_actor_id",
        "owner_service_account_id",
        "schema",
        "row_count",
        "column_count",
        "byte_count",
        "cell_count",
        "schema_hash",
        "digest",
        "classification",
        "lineage",
        "plan_hash",
        "policy_hash",
        "expires_at",
        "schema_ipc_base64",
        "arrow_schema_hash",
    ];
    if value
        .as_object()
        .is_none_or(|v| v.keys().any(|k| !FIELDS.contains(&k.as_str())))
        || value["row_count"].as_u64().is_none()
        || !["public", "internal", "confidential", "restricted"]
            .contains(&value["classification"].as_str().unwrap_or_default())
    {
        return Err(AppError::Forbidden);
    }
    if value["allows_delegation"] != true
        || value["requires_source_access"] != false
        || value["lineage"].as_array().is_none_or(|rows| {
            rows.is_empty()
                || rows.iter().any(|r| {
                    r.as_object().is_none_or(|v| {
                        v.keys().any(|k| {
                            ![
                                "alias",
                                "connection_id",
                                "snapshot_id",
                                "entities",
                                "scope_binding_hash",
                                "classification",
                                "remote",
                            ]
                            .contains(&k.as_str())
                        })
                    }) || !r.get("remote").is_none_or(Value::is_null)
                })
        })
    {
        return Err(AppError::Forbidden);
    }
    let schema = value["schema_ipc_base64"]
        .as_str()
        .filter(|s| s.len() <= 1024 * 1024)
        .ok_or(AppError::Forbidden)?;
    let decoded = STANDARD.decode(schema).map_err(|_| AppError::Forbidden)?;
    if decoded.is_empty() {
        return Err(AppError::Forbidden);
    }
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(decoded)));
    if value["arrow_schema_hash"] != digest {
        return Err(AppError::Forbidden);
    }
    let expires: DateTime<Utc> =
        serde_json::from_value(value["expires_at"].clone()).map_err(|_| AppError::Forbidden)?;
    if expires <= Utc::now() {
        return Err(AppError::Forbidden);
    }
    columns(&value)?;
    Ok(value)
}

pub fn owner_context(grant: &DatasetDelegation) -> Result<AuthContext, AppError> {
    Ok(AuthContext {
        user_id: Uuid::parse_str(&grant.owner.actor_id).map_err(|_| AppError::Forbidden)?,
        org_id: grant.owner.tenant_id,
        is_oidc: false,
        is_mcp_peer: false,
        active_role_id: None,
        session_id: None,
        mfa_verified: false,
        is_service_account: false,
        service_account_token_id: None,
        permissions: vec![],
    })
}

pub async fn validate_grant(
    state: &AppState,
    grant: &DatasetDelegation,
) -> Result<Value, AppError> {
    if grant.expires_at <= Utc::now() || grant.revoked_at.is_some() {
        return Err(AppError::Unauthorized);
    }
    let ctx = owner_context(grant)?;
    if owner_identity(state, &ctx, grant.owner.workspace_id).await? != grant.owner {
        return Err(AppError::Forbidden);
    }
    let value = manifest(
        state,
        &ctx,
        grant.owner.workspace_id,
        grant.dataset_id,
        grant.version_id,
    )
    .await?;
    if value["digest"] != grant.content_digest
        || value["arrow_schema_hash"] != grant.arrow_schema_hash
        || columns(&value)? != grant.columns
    {
        return Err(AppError::Forbidden);
    }
    Ok(value)
}

pub fn columns(value: &Value) -> Result<Vec<String>, AppError> {
    let fields = value["schema"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 4096)
        .ok_or(AppError::Forbidden)?;
    let names: Vec<String> = fields
        .iter()
        .map(|f| {
            f["name"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 1024)
                .map(str::to_owned)
                .ok_or(AppError::Forbidden)
        })
        .collect::<Result<_, _>>()?;
    if names.iter().collect::<std::collections::HashSet<_>>().len() != names.len() {
        return Err(AppError::Forbidden);
    }
    Ok(names)
}
