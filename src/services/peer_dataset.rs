use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    models::dataset_delegation::DatasetIdentity,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Discover {
    pub peer_id: Uuid,
    pub grant_id: Uuid,
    pub token: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Import {
    pub peer_id: Uuid,
    pub grant_id: Uuid,
    pub token: String,
    pub alias: String,
    pub name: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub grant_id: Uuid,
    pub owner: DatasetIdentity,
    pub origin: DatasetIdentity,
    pub dataset_id: Uuid,
    pub version_id: Uuid,
    pub schema_ipc_base64: String,
    pub arrow_schema_hash: String,
    pub content_digest: String,
    pub row_count: u64,
    pub classification: String,
    pub expires_at: DateTime<Utc>,
    pub columns: Vec<String>,
    pub read_only: bool,
}
#[derive(Clone, sqlx::FromRow)]
pub struct BoundPeer {
    pub id: Uuid,
    pub name: String,
    pub mcp_url: String,
    pub binding_id: Uuid,
    pub foreign_tenant_id: String,
    pub foreign_workspace_id: Option<String>,
    pub sharing_policy: serde_json::Value,
}

pub async fn origin(
    state: &AppState,
    ctx: &AuthContext,
    workspace: Uuid,
) -> Result<DatasetIdentity, AppError> {
    ensure_workspace_in_org(&state.pool, workspace, ctx.org_id).await?;
    require_permission(ctx, &state.pool, workspace, "data:query").await?;
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
    .map_err(anyhow::Error::from)?;
    if !member {
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

pub async fn bound(
    state: &AppState,
    ctx: &AuthContext,
    workspace: Uuid,
    peer: Uuid,
) -> Result<BoundPeer, AppError> {
    origin(state, ctx, workspace).await?;
    crate::routes::relay::authorize_source_admin(state, ctx, workspace).await?;
    let mut tx = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    let peer=sqlx::query_as("SELECT p.id,p.name,p.mcp_url,b.id AS binding_id,b.foreign_tenant_id,b.foreign_workspace_id,p.sharing_policy FROM peers p JOIN workspace_peer_bindings b ON b.peer_id=p.id AND b.org_id=p.org_id WHERE p.id=$1 AND p.org_id=$2 AND b.workspace_id=$3 AND p.status='active' AND b.status='active'")
        .bind(peer).bind(ctx.org_id).bind(workspace).fetch_optional(&mut *tx).await.map_err(anyhow::Error::from)?.ok_or(AppError::Forbidden)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(peer)
}

pub async fn identity(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ws): Path<Uuid>,
) -> Result<Json<DatasetIdentity>, AppError> {
    Ok(Json(origin(&state, &ctx, ws).await?))
}
pub async fn choices(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ws): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    origin(&state, &ctx, ws).await?;
    crate::routes::relay::authorize_source_admin(&state, &ctx, ws).await?;
    let mut tx = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    let peers:Vec<(Uuid,String,Uuid)>=sqlx::query_as("SELECT p.id,p.name,b.id FROM peers p JOIN workspace_peer_bindings b ON b.peer_id=p.id AND b.org_id=p.org_id WHERE p.org_id=$1 AND b.workspace_id=$2 AND p.status='active' AND b.status='active' ORDER BY p.name LIMIT 100").bind(ctx.org_id).bind(ws).fetch_all(&mut *tx).await.map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(Json(
        serde_json::json!({"peers":peers.into_iter().map(|(id,name,binding)|serde_json::json!({"id":id,"name":name,"bindingId":binding})).collect::<Vec<_>>()}),
    ))
}

pub async fn discover(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ws): Path<Uuid>,
    Json(input): Json<Discover>,
) -> Result<Json<Descriptor>, AppError> {
    let peer = bound(&state, &ctx, ws, input.peer_id).await?;
    let identity = origin(&state, &ctx, ws).await?;
    let (_, descriptor) = fetch(&state, &peer, &identity, input.grant_id, &input.token).await?;
    let fresh = bound(&state, &ctx, ws, input.peer_id).await?;
    if fresh.binding_id != peer.binding_id || fresh.mcp_url != peer.mcp_url {
        return Err(AppError::Forbidden);
    }
    Ok(Json(descriptor))
}

pub async fn fetch(
    state: &AppState,
    peer: &BoundPeer,
    origin: &DatasetIdentity,
    grant: Uuid,
    token: &str,
) -> Result<(String, Descriptor), AppError> {
    if token.len() != 51 || !token.starts_with("ione_dg_") {
        return Err(AppError::BadRequest("invalid dataset credential".into()));
    }
    let mut url = url::Url::parse(&peer.mcp_url).map_err(|_| AppError::Forbidden)?;
    if !["", "/", "/mcp", "/mcp/"].contains(&url.path())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Forbidden);
    }
    let host = url
        .host_str()
        .ok_or(AppError::Forbidden)?
        .trim_matches(['[', ']'])
        .to_owned();
    let private = state.config.allow_private_peers
        && state
            .config
            .private_peer_allowlist
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host));
    if url.scheme() != "https" && !(url.scheme() == "http" && private) {
        return Err(AppError::Forbidden);
    }
    let port = url.port_or_known_default().ok_or(AppError::Forbidden)?;
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| AppError::Forbidden)?
    .map_err(|_| AppError::Forbidden)?
    .collect();
    if (url.scheme() == "http" && addresses.iter().any(|a| !a.ip().is_loopback()))
        || addresses.is_empty()
        || addresses.len() > 32
        || addresses.iter().any(|a| !allowed_ip(a.ip(), private))
    {
        return Err(AppError::Forbidden);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addresses)
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| AppError::Forbidden)?;
    url.set_path("");
    let base = url.as_str().trim_end_matches('/').to_owned();
    url.set_path(&format!("/api/v1/dataset-delegations/{grant}/descriptor"));
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .header(
            "x-ione-dataset-origin",
            serde_json::to_string(origin).map_err(anyhow::Error::from)?,
        )
        .send()
        .await
        .map_err(|_| AppError::RelayUpstream("peer dataset unavailable".into()))?;
    if response.status() != reqwest::StatusCode::OK
        || response.content_length().is_some_and(|n| n > 65536)
    {
        return Err(AppError::Forbidden);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::RelayUpstream("peer dataset interrupted".into()))?
    {
        if chunk.len() > 65536 - body.len() {
            return Err(AppError::Forbidden);
        }
        body.extend_from_slice(&chunk);
    }
    let d: Descriptor = serde_json::from_slice(&body).map_err(|_| AppError::Forbidden)?;
    let schema = STANDARD
        .decode(&d.schema_ipc_base64)
        .map_err(|_| AppError::Forbidden)?;
    let hash = format!("sha256:{}", hex::encode(Sha256::digest(&schema)));
    if d.owner.deployment_id.is_nil()
        || d.owner.tenant_id.is_nil()
        || d.owner.workspace_id.is_nil()
        || [&d.owner.actor_id, &d.owner.service_account_id]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        || &d.origin != origin
        || d.owner.deployment_id == origin.deployment_id
        || d.grant_id != grant
        || d.dataset_id.is_nil()
        || d.version_id.is_nil()
        || !d.read_only
        || d.expires_at <= Utc::now()
        || schema.is_empty()
        || d.arrow_schema_hash != hash
        || d.content_digest.len() != 71
        || !d.content_digest.starts_with("sha256:")
        || !d.content_digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
        || d.columns.is_empty()
        || d.columns.len() > 4096
        || d.columns.iter().any(|c| c.is_empty() || c.len() > 1024)
        || d.columns
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != d.columns.len()
        || !["public", "internal", "confidential", "restricted"]
            .contains(&d.classification.as_str())
    {
        return Err(AppError::Forbidden);
    }
    if peer.foreign_tenant_id != d.owner.tenant_id.to_string()
        || peer
            .foreign_workspace_id
            .as_ref()
            .is_some_and(|w| w != &d.owner.workspace_id.to_string())
    {
        return Err(AppError::Forbidden);
    }
    if let Some(pin) = peer.sharing_policy.get("deployment_id") {
        if pin.as_str() != Some(d.owner.deployment_id.to_string().as_str()) {
            return Err(AppError::Forbidden);
        }
    }
    Ok((base, d))
}

fn allowed_ip(ip: IpAddr, private: bool) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            !ip.is_unspecified()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && !(o[0] == 198 && (o[1] == 18 || o[1] == 19))
                && !(o[0] == 192 && o[1] == 0 && o[2] == 0)
                && o[0] != 0
                && o[0] < 240
                && !(o[0] == 100 && (64..128).contains(&o[1]))
                && (private || (!ip.is_loopback() && !ip.is_private()))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return allowed_ip(IpAddr::V4(v4), private);
            }
            let segments = ip.segments();
            if ip.is_loopback() {
                return private;
            }
            if segments[0] & 0xfe00 == 0xfc00 {
                return private && !(segments[0] == 0xfd00 && segments[1] == 0x0ec2);
            }
            segments[0] & 0xe000 == 0x2000
                && segments[0] != 0x2002
                && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
        }
    }
}

fn matches_import(peer: &BoundPeer, original: &BoundPeer, descriptor: &Descriptor) -> bool {
    peer.id == original.id
        && peer.binding_id == original.binding_id
        && peer.mcp_url == original.mcp_url
        && peer.foreign_tenant_id == descriptor.owner.tenant_id.to_string()
        && peer.foreign_workspace_id.as_deref()
            == Some(descriptor.owner.workspace_id.to_string().as_str())
        && peer
            .sharing_policy
            .get("deployment_id")
            .is_none_or(|p| p.as_str() == Some(descriptor.owner.deployment_id.to_string().as_str()))
}

pub async fn import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ws): Path<Uuid>,
    Json(input): Json<Import>,
) -> Result<Json<crate::models::WorkspaceRelayMapping>, AppError> {
    if !crate::models::alias_is_valid(&input.alias)
        || input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest("invalid source name or alias".into()));
    }
    let peer = bound(&state, &ctx, ws, input.peer_id).await?;
    let identity = origin(&state, &ctx, ws).await?;
    let (base, d) = fetch(&state, &peer, &identity, input.grant_id, &input.token).await?;
    let fresh = bound(&state, &ctx, ws, input.peer_id).await?;
    if !matches_import(&fresh, &peer, &d) {
        return Err(AppError::Forbidden);
    }
    let remote = serde_json::json!({"grant_id":d.grant_id,"owner":d.owner,"origin":d.origin,"dataset_id":d.dataset_id,"version_id":d.version_id,"arrow_schema_hash":d.arrow_schema_hash,"content_digest":d.content_digest,"expires_at":d.expires_at});
    let config = serde_json::json!({"base_url":base,"remote":remote,"schema_ipc_base64":d.schema_ipc_base64,"row_count":d.row_count,"columns":d.columns,"classification":d.classification});
    let client = state
        .relay
        .as_deref()
        .filter(|c| c.source_management_enabled())
        .ok_or(AppError::Forbidden)?;
    let mut intent = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    sqlx::query("INSERT INTO relay_source_registrations(org_id,workspace_id,actor_id,alias) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(ctx.org_id).bind(ws).bind(ctx.user_id).bind(&input.alias).execute(&mut *intent).await.map_err(anyhow::Error::from)?;
    intent.commit().await.map_err(anyhow::Error::from)?;
    let mut tx = crate::rls::org_scoped_tx(&state.pool, ctx.org_id).await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "relay-source:{}:{}:{}",
                ctx.org_id, ws, input.alias
            ))
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    if !locked {
        return Err(AppError::ConflictJson(
            serde_json::json!({"error":"registration_in_progress"}),
        ));
    }
    let repo = crate::repos::RelayMappingRepo::new(state.pool.clone());
    if let Some(mapping) = repo
        .by_alias(ctx.org_id, ws, &input.alias)
        .await
        .map_err(AppError::Internal)?
    {
        let same:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM relay_peer_dataset_mappings WHERE mapping_id=$1 AND org_id=$2 AND workspace_id=$3 AND peer_id=$4 AND binding_id=$5 AND grant_id=$6)").bind(mapping.id).bind(ctx.org_id).bind(ws).bind(peer.id).bind(peer.binding_id).bind(d.grant_id).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
        if !same || mapping.principal_id != Some(ctx.user_id) || !mapping.enabled {
            return Err(AppError::BadRequest("source alias already exists".into()));
        }
        let unpinned:bool=sqlx::query_scalar("SELECT owner_tenant_id IS NULL FROM relay_peer_dataset_mappings WHERE mapping_id=$1 AND org_id=$2 AND workspace_id=$3").bind(mapping.id).bind(ctx.org_id).bind(ws).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
        if unpinned {
            let registered = client
                .manage_source(
                    reqwest::Method::GET,
                    &format!("/v1/connections/{}", mapping.relay_connection_id),
                    &crate::routes::relay::scope_for(&ctx, ws),
                    serde_json::json!({}),
                )
                .await
                .map_err(|_| AppError::Forbidden)?;
            if registered["id"] != serde_json::json!(mapping.relay_connection_id)
                || registered["kind"] != "ione_dataset"
                || registered["public_config"] != config
            {
                return Err(AppError::Forbidden);
            }
            let checked = bound(&state, &ctx, ws, peer.id).await?;
            if !matches_import(&checked, &peer, &d) {
                return Err(AppError::Forbidden);
            }
        }
        let pinned=sqlx::query("UPDATE relay_peer_dataset_mappings d SET owner_tenant_id=$7,owner_workspace_id=$8,owner_deployment_id=$9,peer_url=$10 WHERE d.mapping_id=$1 AND d.org_id=$2 AND d.workspace_id=$3 AND d.peer_id=$4 AND d.binding_id=$5 AND d.grant_id=$6 AND (d.owner_tenant_id IS NULL OR (d.owner_tenant_id=$7 AND d.owner_workspace_id=$8 AND d.owner_deployment_id=$9 AND d.peer_url=$10)) AND EXISTS (SELECT 1 FROM workspace_peer_bindings b JOIN peers p ON p.id=b.peer_id AND p.org_id=b.org_id WHERE b.id=d.binding_id AND b.org_id=d.org_id AND b.workspace_id=d.workspace_id AND b.peer_id=d.peer_id AND b.status='active' AND p.status='active' AND b.foreign_tenant_id=$7::uuid::text AND b.foreign_workspace_id=$8::uuid::text AND p.mcp_url=$10 AND (NOT (p.sharing_policy ? 'deployment_id') OR p.sharing_policy->>'deployment_id'=$9::uuid::text) FOR SHARE OF b,p)")
            .bind(mapping.id).bind(ctx.org_id).bind(ws).bind(peer.id).bind(peer.binding_id).bind(d.grant_id).bind(d.owner.tenant_id).bind(d.owner.workspace_id).bind(d.owner.deployment_id).bind(&peer.mcp_url).execute(&mut *tx).await.map_err(anyhow::Error::from)?.rows_affected();
        if pinned != 1 {
            return Err(AppError::Forbidden);
        }
        tx.commit().await.map_err(anyhow::Error::from)?;
        return Ok(Json(mapping));
    }
    let request_id:Uuid=sqlx::query_scalar("SELECT request_id FROM relay_source_registrations WHERE org_id=$1 AND workspace_id=$2 AND actor_id=$3 AND alias=$4").bind(ctx.org_id).bind(ws).bind(ctx.user_id).bind(&input.alias).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
    let scope = crate::routes::relay::scope_for(&ctx, ws);
    let unavailable = |_| {
        AppError::RelayUpstream(
            "peer source registration was not confirmed; retry the same alias".into(),
        )
    };
    let checked = bound(&state, &ctx, ws, peer.id).await?;
    if !matches_import(&checked, &peer, &d) {
        return Err(AppError::Forbidden);
    }
    let created=client.manage_source(reqwest::Method::POST,"/v1/connections",&scope,serde_json::json!({"request_id":request_id,"workspace_id":ws,"kind":"ione_dataset","name":input.name,"public_config":config,"credential":input.token})).await.map_err(unavailable)?;
    let id = created["id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::RelayUpstream("peer source identity unavailable".into()))?;
    let result=async {
        let current=bound(&state,&ctx,ws,peer.id).await?; if !matches_import(&current,&peer,&d){return Err(AppError::Forbidden)}
        if current.binding_id!=peer.binding_id||current.mcp_url!=peer.mcp_url{return Err(AppError::Forbidden)}
        let validation=client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/validate"),&scope,serde_json::json!({"claimed_assurance":"provider_verified"})).await.map_err(unavailable)?;
        if validation["state"]!="active"{return Err(AppError::BadRequest("peer dataset validation failed".into()))}
        let checked=bound(&state,&ctx,ws,peer.id).await?; if !matches_import(&checked,&peer,&d){return Err(AppError::Forbidden)}
        let catalog=client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/catalog"),&scope,serde_json::json!({})).await.map_err(unavailable)?;
        if catalog["entities"]!=1{return Err(AppError::BadRequest("peer dataset catalog invalid".into()))}
        let checked=bound(&state,&ctx,ws,peer.id).await?; if !matches_import(&checked,&peer,&d){return Err(AppError::Forbidden)}
        client.manage_source(reqwest::Method::POST,&format!("/v1/connections/{id}/grants"),&scope,serde_json::json!({"workspace_id":ws,"principal_kind":"principal","principal_id":ctx.user_id.to_string(),"entity_allowlist":["dataset"]})).await.map_err(unavailable)?;
        let current=bound(&state,&ctx,ws,peer.id).await?; if !matches_import(&current,&peer,&d){return Err(AppError::Forbidden)}
        if current.binding_id!=peer.binding_id||current.mcp_url!=peer.mcp_url{return Err(AppError::Forbidden)}
        let (_,confirmed)=fetch(&state,&current,&identity,d.grant_id,&input.token).await?;
        if serde_json::to_value(&confirmed).map_err(anyhow::Error::from)?!=serde_json::to_value(&d).map_err(anyhow::Error::from)?{return Err(AppError::Forbidden)}
        let checked=bound(&state,&ctx,ws,peer.id).await?; if !matches_import(&checked,&peer,&d){return Err(AppError::Forbidden)}
        let binding_matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_peer_bindings b JOIN peers p ON p.id=b.peer_id AND p.org_id=b.org_id WHERE b.id=$1 AND b.org_id=$2 AND b.workspace_id=$3 AND b.peer_id=$4 AND b.status='active' AND p.status='active' AND b.foreign_tenant_id=$5::uuid::text AND b.foreign_workspace_id=$6::uuid::text AND p.mcp_url=$7 AND (NOT (p.sharing_policy ? 'deployment_id') OR p.sharing_policy->>'deployment_id'=$8::uuid::text) FOR SHARE OF b,p)").bind(peer.binding_id).bind(ctx.org_id).bind(ws).bind(peer.id).bind(d.owner.tenant_id).bind(d.owner.workspace_id).bind(&peer.mcp_url).bind(d.owner.deployment_id).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
        if !binding_matches {return Err(AppError::Forbidden)}
        let mapping:crate::models::WorkspaceRelayMapping=sqlx::query_as("INSERT INTO workspace_relay_mappings(org_id,workspace_id,relay_connection_id,display_name,alias,principal_id,created_by,peer_dataset_import) VALUES($1,$2,$3,$4,$5,$6,$6,true) RETURNING id,org_id,workspace_id,relay_connection_id,display_name,alias,principal_id,enabled,created_by,created_at,updated_at").bind(ctx.org_id).bind(ws).bind(id).bind(&input.name).bind(&input.alias).bind(ctx.user_id).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
        sqlx::query("INSERT INTO relay_peer_dataset_mappings(mapping_id,org_id,workspace_id,peer_id,binding_id,grant_id,owner_tenant_id,owner_workspace_id,owner_deployment_id,peer_url) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(mapping.id).bind(ctx.org_id).bind(ws).bind(peer.id).bind(peer.binding_id).bind(d.grant_id).bind(d.owner.tenant_id).bind(d.owner.workspace_id).bind(d.owner.deployment_id).bind(&peer.mcp_url).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
        Ok(mapping)
    }.await;
    if matches!(
        result,
        Err(AppError::Forbidden) | Err(AppError::BadRequest(_))
    ) && client
        .manage_source(
            reqwest::Method::DELETE,
            &format!("/v1/connections/{id}"),
            &scope,
            serde_json::json!({}),
        )
        .await
        .is_ok()
    {
        sqlx::query("DELETE FROM relay_source_registrations WHERE org_id=$1 AND workspace_id=$2 AND actor_id=$3 AND alias=$4 AND request_id=$5").bind(ctx.org_id).bind(ws).bind(ctx.user_id).bind(&input.alias).bind(request_id).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        return result.map(Json);
    }
    let mapping = result?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(Json(mapping))
}

#[cfg(test)]
mod tests {
    use super::allowed_ip;
    #[test]
    fn peer_dataset_network_bounds_are_explicit() {
        for address in [
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "fe80::1",
            "fd00:ec2::254",
            "::ffff:169.254.169.254",
            "2001:db8::1",
        ] {
            assert!(!allowed_ip(address.parse().unwrap(), false), "{address}");
            assert!(!allowed_ip(address.parse().unwrap(), true), "{address}");
        }
        for address in ["127.0.0.1", "10.0.0.1", "::1", "fd12::1"] {
            assert!(!allowed_ip(address.parse().unwrap(), false));
            assert!(allowed_ip(address.parse().unwrap(), true));
        }
        for address in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(allowed_ip(address.parse().unwrap(), false));
        }
    }
}
