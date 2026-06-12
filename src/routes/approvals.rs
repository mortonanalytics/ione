use axum::{
    extract::{Extension, Path, Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{ensure_workspace_in_org, require_permission, AuthContext},
    error::AppError,
    middleware::session_cookie::SessionId,
    models::{ActivationStepKey, ActivationTrack, ActorKind, ApprovalStatus},
    repos::{ApprovalRepo, AuditEventRepo},
    services::delivery,
    state::AppState,
};

#[derive(Deserialize)]
pub struct ApprovalsQuery {
    pub status: Option<String>,
}

/// GET /api/v1/workspaces/:id/approvals?status=pending|approved|rejected
pub async fn list_approvals(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ApprovalsQuery>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;
    let status_filter = query.status.as_deref().and_then(parse_status);
    let repo = ApprovalRepo::new(state.pool.clone());
    let items = repo
        .list(workspace_id, status_filter)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct DecideRequest {
    pub decision: String,
    pub comment: Option<String>,
}

/// POST /api/v1/approvals/:id
///
/// Body: `{ decision: "approved" | "rejected", comment?: string }`
/// Returns: `Approval`
pub async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(session): Extension<SessionId>,
    Json(req): Json<DecideRequest>,
) -> Result<Json<Value>, AppError> {
    let decision = parse_status(&req.decision)
        .filter(|s| *s != ApprovalStatus::Pending)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "decision must be 'approved' or 'rejected', got '{}'",
                req.decision
            ))
        })?;

    let approval_repo = ApprovalRepo::new(state.pool.clone());
    let audit_repo = AuditEventRepo::new(state.pool.clone());

    // Get the approval to resolve workspace_id via the artifact.
    let existing = approval_repo
        .get(approval_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest(format!("approval {} not found", approval_id)))?;

    // Resolve workspace_id from the artifact.
    let workspace_id: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM artifacts WHERE id = $1")
            .bind(existing.artifact_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                AppError::Internal(anyhow::anyhow!("failed to resolve workspace: {}", e))
            })?;

    // Gate before any path that can trigger external tool execution or
    // delivery (H-1). Fail-closed when the artifact (NOT NULL workspace_id)
    // is missing entirely.
    match workspace_id {
        Some(workspace_id) => {
            ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;
            require_permission(&auth, &state.pool, workspace_id, "approvals:decide").await?;
        }
        None => return Err(AppError::Forbidden),
    }

    // If the approval is already in a terminal state matching the request, return it
    // as-is after draining any pending peer tool-call side effect that may have
    // failed after the approval row was decided.
    if existing.status == decision {
        if let Some(workspace_id) = workspace_id {
            ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;
        }
        if decision == ApprovalStatus::Approved {
            let _ = crate::services::federation::execute_pending_tool_call(
                &state,
                approval_id,
                auth.user_id,
            )
            .await
            .map_err(AppError::Internal)?;
        } else if decision == ApprovalStatus::Rejected {
            let _ = crate::services::federation::reject_pending_tool_call(
                &state,
                approval_id,
                auth.user_id,
            )
            .await
            .map_err(AppError::Internal)?;
        }
        return Ok(Json(
            serde_json::to_value(&existing).map_err(|e| AppError::Internal(e.into()))?,
        ));
    }

    if let Some(workspace_id) = workspace_id {
        ensure_workspace_in_org(&state.pool, workspace_id, auth.org_id).await?;
    }

    // Decide the approval (only updates if currently pending).
    let approval = approval_repo
        .decide(
            approval_id,
            auth.user_id,
            decision.clone(),
            req.comment.as_deref(),
        )
        .await
        .map_err(AppError::Internal)?;

    // If the row was already decided (race), return without re-auditing.
    if approval.status != decision {
        return Ok(Json(
            serde_json::to_value(&approval).map_err(|e| AppError::Internal(e.into()))?,
        ));
    }

    // Write the approval/rejection audit row.
    let verb = match decision {
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Rejected => "rejected",
        ApprovalStatus::Pending => unreachable!(),
    };

    audit_repo
        .insert(
            workspace_id,
            ActorKind::User,
            &auth.user_id.to_string(),
            verb,
            "approval",
            Some(approval_id),
            serde_json::json!({ "artifact_id": existing.artifact_id }),
        )
        .await
        .map_err(AppError::Internal)?;

    if let Some(workspace_id) = workspace_id {
        if workspace_id != crate::demo::DEMO_WORKSPACE_ID {
            let activation_repo = crate::repos::ActivationRepo::new(state.pool.clone());
            let was_complete = activation_repo
                .is_step_complete(
                    auth.user_id,
                    workspace_id,
                    ActivationTrack::RealActivation,
                    ActivationStepKey::FirstApprovalDecided,
                )
                .await
                .unwrap_or(false);
            let inserted = activation_repo
                .mark(
                    auth.user_id,
                    workspace_id,
                    ActivationTrack::RealActivation,
                    ActivationStepKey::FirstApprovalDecided,
                )
                .await
                .unwrap_or(false);
            if inserted
                && activation_repo
                    .is_track_complete(auth.user_id, workspace_id, ActivationTrack::RealActivation)
                    .await
                    .unwrap_or(false)
            {
                crate::services::funnel::track(
                    &state,
                    session.0,
                    Some(auth.user_id),
                    Some(workspace_id),
                    "activation_completed",
                    Some(json!({ "track": "real_activation" })),
                );
            }
            if !was_complete {
                crate::services::funnel::track(
                    &state,
                    session.0,
                    Some(auth.user_id),
                    Some(workspace_id),
                    "first_real_approval_decided",
                    Some(json!({
                        "approvalId": approval_id,
                        "decision": req.decision,
                    })),
                );
            }
        }
    }

    if decision == ApprovalStatus::Rejected
        && crate::services::federation::reject_pending_tool_call(&state, approval_id, auth.user_id)
            .await
            .map_err(AppError::Internal)?
    {
        return Ok(Json(
            serde_json::to_value(&approval).map_err(|e| AppError::Internal(e.into()))?,
        ));
    }

    if decision == ApprovalStatus::Approved
        && crate::services::federation::execute_pending_tool_call(&state, approval_id, auth.user_id)
            .await
            .map_err(AppError::Internal)?
            .is_some()
    {
        return Ok(Json(
            serde_json::to_value(&approval).map_err(|e| AppError::Internal(e.into()))?,
        ));
    }

    // On approval, deliver artifact-backed approval types.
    if decision == ApprovalStatus::Approved {
        delivery::deliver_artifact(&state, existing.artifact_id, auth.user_id)
            .await
            .map_err(AppError::Internal)?;
    }

    Ok(Json(
        serde_json::to_value(&approval).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

fn parse_status(s: &str) -> Option<ApprovalStatus> {
    match s.to_lowercase().as_str() {
        "pending" => Some(ApprovalStatus::Pending),
        "approved" => Some(ApprovalStatus::Approved),
        "rejected" => Some(ApprovalStatus::Rejected),
        _ => None,
    }
}
