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
    auth::{
        permission_grants, random_url_safe_string, require_org_permission, sha256_hex, AuthContext,
        SAT_TOKEN_PREFIX,
    },
    error::AppError,
    models::ActorKind,
    repos::{AuditEventRepo, ServiceAccountTokenRepo},
    routes::roles::is_valid_workspace_permission,
    state::AppState,
};

/// Org-scoped permission strings a token may carry (in addition to the
/// workspace vocabulary validated by `is_valid_workspace_permission`).
const ORG_VOCABULARY: &[&str] = &[
    "trust_issuers:manage",
    "peers:manage",
    "service_accounts:manage",
    "provisioning:apply",
];

/// A token permission must be a member of the closed workspace vocabulary or
/// the org vocabulary.
fn is_valid_token_permission(s: &str) -> bool {
    ORG_VOCABULARY.contains(&s) || is_valid_workspace_permission(s)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTokenBody {
    pub name: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub provisionable_max_coc: i32,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn escalation_error(detail: String) -> AppError {
    AppError::ConflictJson(json!({
        "error": "permission_escalation",
        "message": detail,
    }))
}

/// The issuer's authority for the subset/coc escalation guard. A session actor
/// holding `admin` is exempt; a service-account issuer is never exempt.
struct IssuerAuthority {
    held: HashSet<String>,
    max_coc: i32,
    admin_exempt: bool,
}

async fn issuer_authority(
    state: &AppState,
    ctx: &AuthContext,
) -> Result<IssuerAuthority, AppError> {
    if ctx.is_service_account {
        let max_coc = match ctx.service_account_token_id {
            Some(id) => sqlx::query_scalar::<_, i32>(
                "SELECT provisionable_max_coc FROM service_account_tokens WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .unwrap_or(0),
            None => 0,
        };
        return Ok(IssuerAuthority {
            held: ctx.permissions.iter().cloned().collect(),
            max_coc,
            admin_exempt: false,
        });
    }

    // Session actor: union of org-scoped grants and effective workspace grants
    // across every workspace they belong to in this org, plus the max coc.
    let mut held: HashSet<String> = crate::repos::OrgMembershipRepo::new(state.pool.clone())
        .org_permissions(ctx.user_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;

    let (perms, max_coc): (Option<Value>, i32) = sqlx::query_as(
        "SELECT jsonb_agg(r.permissions) AS perms, COALESCE(MAX(r.coc_level), 0) AS max_coc
         FROM memberships m
              JOIN roles r ON r.id = m.role_id
              JOIN workspaces w ON w.id = m.workspace_id
         WHERE m.user_id = $1 AND w.org_id = $2",
    )
    .bind(ctx.user_id)
    .bind(ctx.org_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    if let Some(Value::Array(sets)) = perms {
        for set in sets {
            if let Value::Array(items) = set {
                for item in items {
                    if let Value::String(s) = item {
                        held.insert(s);
                    }
                }
            }
        }
    }
    let admin_exempt = held.contains("admin");
    Ok(IssuerAuthority {
        held,
        max_coc,
        admin_exempt,
    })
}

/// POST /api/v1/service-account-tokens
pub async fn issue(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<IssueTokenBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    require_org_permission(&ctx, &state.pool, "service_accounts:manage").await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;

    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("name must not be empty".into()));
    }
    if body.provisionable_max_coc < 0 || body.provisionable_max_coc > 100 {
        return Err(AppError::BadRequest(
            "provisionable_max_coc must be 0..=100".into(),
        ));
    }
    for p in &body.permissions {
        if !is_valid_token_permission(p) {
            return Err(AppError::UnprocessableEntityJson(json!({
                "error": "invalid_permission",
                "message": format!("'{p}' is not in the permission vocabulary"),
            })));
        }
    }

    // Escalation guard: a token can only confer what the issuer holds.
    let authority = issuer_authority(&state, &ctx).await?;
    if !authority.admin_exempt {
        for p in &body.permissions {
            if !permission_grants(&authority.held, p) {
                return Err(escalation_error(format!(
                    "cannot grant '{p}': you do not hold it"
                )));
            }
        }
        if body.provisionable_max_coc > authority.max_coc {
            return Err(escalation_error(format!(
                "provisionable_max_coc {} is above your effective max {}",
                body.provisionable_max_coc, authority.max_coc
            )));
        }
    }

    let plaintext = format!("{}{}", SAT_TOKEN_PREFIX, random_url_safe_string());
    let token_hash = sha256_hex(&plaintext);
    let permissions = json!(body.permissions);

    let created_by = (!ctx.user_id.is_nil()).then_some(ctx.user_id);
    let token = ServiceAccountTokenRepo::new(state.pool.clone())
        .issue(
            ctx.org_id,
            body.name.trim(),
            &token_hash,
            &permissions,
            body.provisionable_max_coc,
            created_by,
            body.expires_at,
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("duplicate") || e.to_string().contains("unique") {
                AppError::ConflictJson(json!({
                    "error": "duplicate_name",
                    "message": "a token with this name already exists in the org",
                }))
            } else {
                AppError::Internal(e)
            }
        })?;

    AuditEventRepo::new(state.pool.clone())
        .insert(
            None,
            actor_kind(&ctx),
            &actor_ref(&ctx),
            "service_account_token.issued",
            "service_account_token",
            Some(token.id),
            json!({
                "org_id": ctx.org_id,
                "name": token.name,
                "permissions": token.permissions,
                "provisionable_max_coc": token.provisionable_max_coc,
                "expires_at": token.expires_at,
            }),
        )
        .await
        .map_err(AppError::Internal)?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": token.id,
            "token": plaintext,
            "name": token.name,
            "permissions": token.permissions,
            "provisionable_max_coc": token.provisionable_max_coc,
            "expires_at": token.expires_at,
        })),
    ))
}

/// GET /api/v1/service-account-tokens
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Value>, AppError> {
    require_org_permission(&ctx, &state.pool, "service_accounts:manage").await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let items = ServiceAccountTokenRepo::new(state.pool.clone())
        .list_active(ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    // ServiceAccountToken skips token_hash on serialization.
    Ok(Json(json!({ "items": items })))
}

/// DELETE /api/v1/service-account-tokens/:tokenId
pub async fn revoke(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(token_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    require_org_permission(&ctx, &state.pool, "service_accounts:manage").await?;
    crate::routes::mfa_gate(&ctx, &state.pool).await?;
    let revoked = ServiceAccountTokenRepo::new(state.pool.clone())
        .revoke(token_id, ctx.org_id)
        .await
        .map_err(AppError::Internal)?;
    if !revoked {
        return Err(AppError::NotFound("token not found".into()));
    }
    AuditEventRepo::new(state.pool.clone())
        .insert(
            None,
            actor_kind(&ctx),
            &actor_ref(&ctx),
            "service_account_token.revoked",
            "service_account_token",
            Some(token_id),
            json!({ "org_id": ctx.org_id }),
        )
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn actor_kind(ctx: &AuthContext) -> ActorKind {
    if ctx.is_service_account {
        ActorKind::ServiceAccount
    } else {
        ActorKind::User
    }
}

fn actor_ref(ctx: &AuthContext) -> String {
    match ctx.service_account_token_id {
        Some(id) => id.to_string(),
        None => ctx.user_id.to_string(),
    }
}
