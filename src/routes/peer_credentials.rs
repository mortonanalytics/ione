//! Pre-broker per-(workspace, peer) static credentials (issue #19).
//!
//! IONe holds one bearer credential per (workspace, peer) and presents it as
//! `Authorization: Bearer <credential>` on outbound MCP requests made in that
//! workspace's scope. Rotation is `PUT` on the same path — a config operation
//! that rewrites the ciphertext, never a schema change.
//!
//! The plaintext is returned exactly once, by `put_credential`. `get_credential`
//! and `list_credentials` return `WorkspacePeerCredential`, which has no secret
//! field, so no read surface can echo it.
//!
//! Brokered identity (#12) supersedes this mode. The header contract is
//! identical in both, so peers do not rebuild when the broker lands.

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
    auth::{ensure_workspace_in_org, random_url_safe_string, require_permission, AuthContext},
    error::AppError,
    models::ActorKind,
    repos::{AuditEventRepo, WorkspacePeerCredentialRepo},
    routes::peers::ensure_workspace_and_peer_in_org,
    state::AppState,
};

const MAX_CREDENTIAL_LEN: usize = 4096;

#[derive(Deserialize)]
pub struct PutCredentialBody {
    /// The credential the peer issued to IONe. Omit to have IONe generate one
    /// (for peers the operator also administers, e.g. another IONe node).
    #[serde(default)]
    pub credential: Option<String>,
}

/// PUT /api/v1/workspaces/:id/peers/:peerId/credential — create or rotate.
/// Returns the plaintext exactly once; no later read returns it.
pub async fn put_credential(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PutCredentialBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    ensure_workspace_and_peer_in_org(&state, workspace_id, peer_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;

    let plaintext = match body.credential {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                return Err(AppError::UnprocessableEntity(
                    "credential must not be empty or whitespace".into(),
                ));
            }
            if trimmed.len() > MAX_CREDENTIAL_LEN {
                return Err(AppError::UnprocessableEntity(format!(
                    "credential must be at most {MAX_CREDENTIAL_LEN} bytes"
                )));
            }
            // A bearer credential travels in an HTTP header; anything that
            // cannot be a header value would fail at send time instead.
            if trimmed
                .bytes()
                .any(|b| b < 0x20 || b == 0x7f || !b.is_ascii())
            {
                return Err(AppError::UnprocessableEntity(
                    "credential must be printable ASCII".into(),
                ));
            }
            trimmed
        }
        None => random_url_safe_string(),
    };

    let created_by = (!ctx.user_id.is_nil()).then_some(ctx.user_id);
    let outcome = WorkspacePeerCredentialRepo::new(state.pool.clone())
        .upsert(workspace_id, peer_id, &plaintext, created_by)
        .await
        .map_err(AppError::Internal)?;

    let verb = if outcome.rotated {
        "peer_credential.rotated"
    } else {
        "peer_credential.created"
    };
    audit(
        &state,
        &ctx,
        workspace_id,
        peer_id,
        verb,
        outcome.credential.id,
    )
    .await?;

    let status = if outcome.rotated {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(json!({
            "id": outcome.credential.id,
            "workspaceId": outcome.credential.workspace_id,
            "peerId": outcome.credential.peer_id,
            "credential": plaintext,
            "createdAt": outcome.credential.created_at,
            "rotatedAt": outcome.credential.rotated_at,
        })),
    ))
}

/// GET /api/v1/workspaces/:id/peers/:peerId/credential — metadata only.
pub async fn get_credential(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_and_peer_in_org(&state, workspace_id, peer_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let credential = WorkspacePeerCredentialRepo::new(state.pool.clone())
        .get(workspace_id, peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer credential not found".into()))?;
    Ok(Json(
        serde_json::to_value(credential).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

/// GET /api/v1/workspaces/:id/peer-credentials — metadata only.
pub async fn list_credentials(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let items = WorkspacePeerCredentialRepo::new(state.pool.clone())
        .list_for_workspace(workspace_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

/// DELETE /api/v1/workspaces/:id/peers/:peerId/credential — outbound auth for
/// this workspace falls back to OAuth / the env bearer on the next request.
pub async fn delete_credential(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, peer_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    ensure_workspace_and_peer_in_org(&state, workspace_id, peer_id, ctx.org_id).await?;
    require_permission(&ctx, &state.pool, workspace_id, "peers:manage").await?;
    let repo = WorkspacePeerCredentialRepo::new(state.pool.clone());
    let credential = repo
        .get(workspace_id, peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("peer credential not found".into()))?;
    repo.delete(workspace_id, peer_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    audit(
        &state,
        &ctx,
        workspace_id,
        peer_id,
        "peer_credential.deleted",
        credential.id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Audit payload carries identifiers only — never the credential, its
/// ciphertext, or its length.
async fn audit(
    state: &AppState,
    ctx: &AuthContext,
    workspace_id: Uuid,
    peer_id: Uuid,
    verb: &str,
    credential_id: Uuid,
) -> Result<(), AppError> {
    let (actor_kind, actor_ref) = if ctx.is_service_account {
        (
            ActorKind::ServiceAccount,
            ctx.service_account_token_id
                .unwrap_or(ctx.user_id)
                .to_string(),
        )
    } else {
        (ActorKind::User, ctx.user_id.to_string())
    };
    AuditEventRepo::new(state.pool.clone())
        .insert(
            Some(workspace_id),
            actor_kind,
            &actor_ref,
            verb,
            "peer_credential",
            Some(credential_id),
            json!({ "workspace_id": workspace_id, "peer_id": peer_id }),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(())
}
