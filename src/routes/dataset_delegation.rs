use crate::{
    auth::AuthContext,
    error::AppError,
    models::dataset_delegation::{
        CreateDelegation, DatasetDelegation, DatasetIdentity, DatasetRead,
    },
    repos::dataset_delegation_repo::DatasetDelegationRepo,
    services::dataset_delegation as service,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace, dataset, version)): Path<(Uuid, Uuid, Uuid)>,
    Json(input): Json<CreateDelegation>,
) -> Result<Response, AppError> {
    let owner = service::owner_identity(&state, &ctx, workspace).await?;
    let origin = input.origin;
    if input.ttl_seconds == 0
        || input.ttl_seconds > 3600
        || origin.deployment_id.is_nil()
        || origin.tenant_id.is_nil()
        || origin.workspace_id.is_nil()
        || origin.deployment_id == owner.deployment_id
        || [&origin.actor_id, &origin.service_account_id]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(AppError::BadRequest(
            "invalid delegation scope or expiry".into(),
        ));
    }
    let value = service::manifest(&state, &ctx, workspace, dataset, version).await?;
    let now = Utc::now();
    let version_expiry: chrono::DateTime<Utc> =
        serde_json::from_value(value["expires_at"].clone()).map_err(|_| AppError::Forbidden)?;
    let expires_at = (now + Duration::seconds(input.ttl_seconds as i64)).min(version_expiry);
    let content_digest = value["digest"]
        .as_str()
        .filter(|s| {
            s.len() == 71
                && s.starts_with("sha256:")
                && s[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
        .ok_or(AppError::Forbidden)?
        .to_owned();
    let grant = DatasetDelegation {
        grant_id: Uuid::new_v4(),
        owner,
        origin,
        dataset_id: dataset,
        version_id: version,
        columns: service::columns(&value)?,
        arrow_schema_hash: value["arrow_schema_hash"]
            .as_str()
            .ok_or(AppError::Forbidden)?
            .to_owned(),
        content_digest,
        expires_at,
        created_at: now,
        revoked_at: None,
    };
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = format!("ione_dg_{}", URL_SAFE_NO_PAD.encode(bytes));
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    service::owner_identity(&state, &ctx, workspace).await?;
    DatasetDelegationRepo(state.pool.clone())
        .insert(&grant, &hash)
        .await
        .map_err(AppError::Internal)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"grant":grant,"token":token})),
    )
        .into_response())
}

pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace, dataset, version)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let owner = service::owner_identity(&state, &ctx, workspace).await?;
    service::manifest(&state, &ctx, workspace, dataset, version).await?;
    let mut grants = DatasetDelegationRepo(state.pool.clone())
        .list(&owner, dataset, version)
        .await
        .map_err(AppError::Internal)?;
    grants.retain(|g| g.owner == owner);
    Ok(Json(json!({"grants":grants})))
}

pub async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace, dataset, version, grant)): Path<(Uuid, Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    let owner = service::owner_identity(&state, &ctx, workspace).await?;
    service::manifest(&state, &ctx, workspace, dataset, version).await?;
    if !DatasetDelegationRepo(state.pool.clone())
        .revoke(&owner, dataset, version, grant)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::NotFound("delegation not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn authenticate(
    state: &AppState,
    id: Uuid,
    headers: &HeaderMap,
) -> Result<DatasetDelegation, AppError> {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || headers.get_all("x-ione-dataset-origin").iter().count() != 1
    {
        return Err(AppError::Unauthorized);
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 51 && v.starts_with("ione_dg_"))
        .ok_or(AppError::Unauthorized)?;
    let origin: DatasetIdentity = serde_json::from_str(
        headers
            .get("x-ione-dataset-origin")
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() <= 2048)
            .ok_or(AppError::Unauthorized)?,
    )
    .map_err(|_| AppError::Unauthorized)?;
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let grant = DatasetDelegationRepo(state.pool.clone())
        .authenticate(id, &hash)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::Unauthorized)?;
    if grant.origin != origin {
        return Err(AppError::Unauthorized);
    }
    Ok(grant)
}

pub async fn descriptor(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let grant = authenticate(&state, id, &headers).await?;
    let value = service::validate_grant(&state, &grant).await?;
    authenticate(&state, id, &headers).await?;
    Ok(([(header::CACHE_CONTROL,"no-store")],Json(json!({"grant_id":grant.grant_id,"owner":grant.owner,"origin":grant.origin,"dataset_id":grant.dataset_id,"version_id":grant.version_id,"schema_ipc_base64":value["schema_ipc_base64"],"arrow_schema_hash":grant.arrow_schema_hash,"content_digest":grant.content_digest,"row_count":value["row_count"],"classification":value["classification"],"expires_at":grant.expires_at,"columns":grant.columns,"read_only":true}))).into_response())
}

pub async fn read(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<DatasetRead>,
) -> Result<Response, AppError> {
    let grant = authenticate(&state, id, &headers).await?;
    if input.max_rows == 0
        || input.max_bytes == 0
        || input.max_bytes > 64 * 1024 * 1024
        || input.limit.is_some_and(|n| n > input.max_rows)
        || input.columns.len() > grant.columns.len()
        || input.columns.iter().any(|c| !grant.columns.contains(c))
        || input
            .columns
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != input.columns.len()
    {
        return Err(AppError::BadRequest(
            "invalid dataset projection or bounds".into(),
        ));
    }
    let value = service::validate_grant(&state, &grant).await?;
    let total = value["row_count"].as_u64().ok_or(AppError::Forbidden)?;
    let expected_rows = input.limit.unwrap_or(total).min(total);
    if expected_rows > input.max_rows {
        return Err(AppError::BadRequest("dataset exceeds row bound".into()));
    }
    let ctx = service::owner_context(&grant)?;
    let result = state
        .relay
        .as_deref()
        .ok_or(AppError::Forbidden)?
        .dataset_read(
            &super::relay::scope_for(&ctx, grant.owner.workspace_id),
            &format!(
                "/v1/datasets/{}/versions/{}/read",
                grant.dataset_id, grant.version_id
            ),
            &input,
        )
        .await
        .map_err(super::relay::map_error)?;
    if result.source_digest != grant.content_digest || result.row_count != expected_rows {
        return Err(AppError::Forbidden);
    }
    let fresh = authenticate(&state, id, &headers).await?;
    service::validate_grant(&state, &fresh).await?;
    authenticate(&state, id, &headers).await?;
    Ok((
        [
            (
                "content-type",
                "application/vnd.apache.arrow.stream".to_owned(),
            ),
            ("cache-control", "no-store".to_owned()),
            ("x-relay-source-digest", result.source_digest),
            ("x-relay-content-digest", result.content_digest),
            ("x-relay-row-count", result.row_count.to_string()),
        ],
        result.bytes,
    )
        .into_response())
}
