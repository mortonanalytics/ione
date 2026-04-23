use anyhow::Context;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    connectors::build_from_row,
    models::{ActorKind, ApprovalStatus, ArtifactKind, ConnectorStatus, PeerStatus},
    repos::{ApprovalRepo, ArtifactRepo, AuditEventRepo, ConnectorRepo, PeerRepo},
    services::{
        auto_exec::{self, AutoExecOutcome},
        peer::check_sharing_policy,
    },
    state::AppState,
};

// ── Actor identity ────────────────────────────────────────────────────────────

/// Who is initiating a delivery action.
#[derive(Clone, Debug)]
pub enum ActorIdent {
    System(&'static str),
    User(Uuid),
}

impl ActorIdent {
    fn kind(&self) -> ActorKind {
        match self {
            ActorIdent::System(_) => ActorKind::System,
            ActorIdent::User(_) => ActorKind::User,
        }
    }

    fn actor_ref(&self) -> String {
        match self {
            ActorIdent::System(s) => s.to_string(),
            ActorIdent::User(id) => id.to_string(),
        }
    }
}

// ── Idempotency guard ─────────────────────────────────────────────────────────

/// Returns true if a terminal audit row already exists for this routing_decision id.
async fn already_processed(pool: &sqlx::PgPool, routing_id: Uuid) -> anyhow::Result<bool> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM audit_events
            WHERE payload->>'routing_id' = $1::text
              AND verb IN ('delivered', 'delivery_failed', 'drafted')
         )",
    )
    .bind(routing_id)
    .fetch_one(pool)
    .await
    .context("failed to check idempotency for routing_decision")?;
    Ok(exists)
}

// ── process_routing_decision ──────────────────────────────────────────────────

/// Process a single routing_decision row end-to-end.
///
/// - `feed`         → no-op (pull-based)
/// - `notification` → resolve connector, invoke send, write audit
/// - `draft`        → create artifact + pending approval, write audit
/// - `peer`         → no-op in Phase 9
///
/// Idempotent: early-returns if a terminal audit row already exists for
/// this routing_id.
pub async fn process_routing_decision(state: &AppState, routing_id: Uuid) -> anyhow::Result<()> {
    if already_processed(&state.pool, routing_id).await? {
        info!(routing_id = %routing_id, "process_routing_decision: already processed, skipping");
        return Ok(());
    }

    // Fetch the routing_decision + signal context.
    let row: Option<(
        String,
        serde_json::Value,
        Uuid,
        String,
        String,
        String,
        Uuid,
    )> = sqlx::query_as(
        "SELECT rd.target_kind::TEXT, rd.target_ref,
                    sig.id AS signal_id, sig.title, sig.body, sig.severity::TEXT,
                    sig.workspace_id
             FROM routing_decisions rd
             JOIN survivors s ON s.id = rd.survivor_id
             JOIN signals sig ON sig.id = s.signal_id
             WHERE rd.id = $1",
    )
    .bind(routing_id)
    .fetch_optional(&state.pool)
    .await
    .context("failed to fetch routing_decision")?;

    let (target_kind, target_ref, _signal_id, signal_title, signal_body, severity, workspace_id) =
        match row {
            Some(r) => r,
            None => {
                warn!(routing_id = %routing_id, "process_routing_decision: routing_decision not found");
                return Ok(());
            }
        };

    // Fetch survivor_id for artifact creation.
    let survivor_id: Uuid =
        sqlx::query_scalar("SELECT survivor_id FROM routing_decisions WHERE id = $1")
            .bind(routing_id)
            .fetch_one(&state.pool)
            .await
            .context("failed to fetch survivor_id from routing_decision")?;

    match target_kind.as_str() {
        "feed" => {
            // Feed is pull-based — nothing to do.
            info!(routing_id = %routing_id, "process_routing_decision: feed target, no-op");
        }
        "notification" => {
            process_notification(
                state,
                routing_id,
                workspace_id,
                &target_ref,
                &signal_title,
                &signal_body,
            )
            .await?;
        }
        "draft" => {
            process_draft(
                state,
                routing_id,
                workspace_id,
                survivor_id,
                &target_ref,
                &signal_title,
                &signal_body,
            )
            .await?;
        }
        "peer" => {
            process_peer(
                state,
                routing_id,
                workspace_id,
                survivor_id,
                &target_ref,
                &signal_title,
                &signal_body,
                &severity,
            )
            .await?;
        }
        other => {
            warn!(routing_id = %routing_id, target_kind = other, "process_routing_decision: unknown target kind");
        }
    }

    Ok(())
}

// ── Notification path ─────────────────────────────────────────────────────────

async fn process_notification(
    state: &AppState,
    routing_id: Uuid,
    workspace_id: Uuid,
    target_ref: &serde_json::Value,
    signal_title: &str,
    signal_body: &str,
) -> anyhow::Result<()> {
    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let audit_repo = AuditEventRepo::new(state.pool.clone());
    let actor = ActorIdent::System("router");

    // Resolve the connector: use target_ref.connector_id, or fall back to the
    // first active notification-capable connector in the workspace.
    let connector = resolve_connector(&connector_repo, workspace_id, target_ref).await?;

    let connector_id = connector.id;
    let text = format!("[IONe Alert] {}: {}", signal_title, signal_body);

    match build_from_row(&connector) {
        Ok(impl_) => {
            match impl_
                .invoke("send", serde_json::json!({ "text": text }))
                .await
            {
                Ok(_) => {
                    info!(
                        routing_id = %routing_id,
                        connector_id = %connector_id,
                        "notification delivered"
                    );
                    audit_repo
                        .insert(
                            Some(workspace_id),
                            actor.kind(),
                            &actor.actor_ref(),
                            "delivered",
                            "connector",
                            Some(connector_id),
                            serde_json::json!({ "routing_id": routing_id }),
                        )
                        .await
                        .context("failed to write delivered audit event")?;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    warn!(
                        routing_id = %routing_id,
                        connector_id = %connector_id,
                        error = %err_msg,
                        "notification delivery failed"
                    );
                    // Write delivery_failed audit row with error context.
                    audit_repo
                        .insert(
                            Some(workspace_id),
                            actor.kind(),
                            &actor.actor_ref(),
                            "delivery_failed",
                            "connector",
                            Some(connector_id),
                            serde_json::json!({
                                "routing_id": routing_id,
                                "error": err_msg,
                                "status_hint": extract_status_code(&err_msg),
                            }),
                        )
                        .await
                        .context("failed to write delivery_failed audit event")?;
                    // Mark connector as error.
                    connector_repo
                        .update_status(connector_id, ConnectorStatus::Error, Some(&err_msg))
                        .await
                        .context("failed to update connector status to error")?;
                }
            }
        }
        Err(e) => {
            let err_msg = format!("failed to build connector: {}", e);
            warn!(routing_id = %routing_id, error = %err_msg, "connector build failed");
            audit_repo
                .insert(
                    Some(workspace_id),
                    actor.kind(),
                    &actor.actor_ref(),
                    "delivery_failed",
                    "connector",
                    Some(connector_id),
                    serde_json::json!({
                        "routing_id": routing_id,
                        "error": err_msg,
                    }),
                )
                .await
                .context("failed to write delivery_failed audit event")?;
            connector_repo
                .update_status(connector_id, ConnectorStatus::Error, Some(&err_msg))
                .await
                .context("failed to update connector status to error")?;
        }
    }

    Ok(())
}

// ── Draft path ────────────────────────────────────────────────────────────────

async fn process_draft(
    state: &AppState,
    routing_id: Uuid,
    workspace_id: Uuid,
    survivor_id: Uuid,
    target_ref: &serde_json::Value,
    signal_title: &str,
    signal_body: &str,
) -> anyhow::Result<()> {
    // Phase 10: try auto-exec first. If a policy matches, skip the approval path
    // entirely — auto_exec writes its own audit trail (auto_authorized +
    // delivered/delivery_failed). Any non-match outcome falls through to the
    // standard draft→approval path.
    match auto_exec::evaluate_and_invoke(state, survivor_id).await {
        Ok(AutoExecOutcome::Delivered { .. }) | Ok(AutoExecOutcome::DeliveryFailed { .. }) => {
            info!(
                routing_id = %routing_id,
                survivor_id = %survivor_id,
                "auto_exec consumed the draft path; skipping approval creation"
            );
            return Ok(());
        }
        Ok(_) => {
            // NoMatch, RateLimited, ConnectorMissing, TemplateError — fall through
            // to the human-approval draft.
        }
        Err(e) => {
            warn!(routing_id = %routing_id, error = %e, "auto_exec evaluation failed; falling back to draft");
        }
    }

    let artifact_repo = ArtifactRepo::new(state.pool.clone());
    let approval_repo = ApprovalRepo::new(state.pool.clone());
    let audit_repo = AuditEventRepo::new(state.pool.clone());
    let actor = ActorIdent::System("router");

    let content = serde_json::json!({
        "title": signal_title,
        "body": signal_body,
        "target_ref": target_ref,
        "routing_id": routing_id,
    });

    let artifact = artifact_repo
        .insert(
            workspace_id,
            ArtifactKind::NotificationDraft,
            Some(survivor_id),
            content,
            None,
        )
        .await
        .context("failed to insert artifact for draft")?;

    let _approval = approval_repo
        .create_pending(artifact.id)
        .await
        .context("failed to create pending approval")?;

    audit_repo
        .insert(
            Some(workspace_id),
            actor.kind(),
            &actor.actor_ref(),
            "drafted",
            "artifact",
            Some(artifact.id),
            serde_json::json!({ "routing_id": routing_id }),
        )
        .await
        .context("failed to write drafted audit event")?;

    info!(
        routing_id = %routing_id,
        artifact_id = %artifact.id,
        "draft artifact created with pending approval"
    );

    Ok(())
}

// ── deliver_artifact ──────────────────────────────────────────────────────────

/// Deliver an already-approved artifact via its connector.
/// Writes a 'delivered' audit row and marks the approval as decided.
pub async fn deliver_artifact(
    state: &AppState,
    artifact_id: Uuid,
    approver_user_id: Uuid,
) -> anyhow::Result<()> {
    let artifact_repo = ArtifactRepo::new(state.pool.clone());
    let approval_repo = ApprovalRepo::new(state.pool.clone());
    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let audit_repo = AuditEventRepo::new(state.pool.clone());

    let artifact = artifact_repo
        .get(artifact_id)
        .await
        .context("failed to fetch artifact")?
        .ok_or_else(|| anyhow::anyhow!("artifact {} not found", artifact_id))?;

    let workspace_id = artifact.workspace_id;
    let actor = ActorIdent::User(approver_user_id);

    // Resolve connector from artifact.content.target_ref.
    let target_ref = artifact
        .content
        .get("target_ref")
        .cloned()
        .unwrap_or_default();
    let connector = resolve_connector(&connector_repo, workspace_id, &target_ref).await?;
    let connector_id = connector.id;

    let title = artifact.content["title"]
        .as_str()
        .unwrap_or("IONe Notification");
    let body = artifact.content["body"].as_str().unwrap_or("");
    let text = format!("[IONe Approved] {}: {}", title, body);

    let build_result = build_from_row(&connector);

    match build_result {
        Ok(impl_) => {
            match impl_
                .invoke("send", serde_json::json!({ "text": text }))
                .await
            {
                Ok(_) => {
                    audit_repo
                        .insert(
                            Some(workspace_id),
                            actor.kind(),
                            &actor.actor_ref(),
                            "delivered",
                            "connector",
                            Some(connector_id),
                            serde_json::json!({ "artifact_id": artifact_id }),
                        )
                        .await
                        .context("failed to write delivered audit event")?;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    audit_repo
                        .insert(
                            Some(workspace_id),
                            actor.kind(),
                            &actor.actor_ref(),
                            "delivery_failed",
                            "connector",
                            Some(connector_id),
                            serde_json::json!({
                                "artifact_id": artifact_id,
                                "error": err_msg,
                            }),
                        )
                        .await
                        .context("failed to write delivery_failed audit event")?;
                    connector_repo
                        .update_status(connector_id, ConnectorStatus::Error, Some(&err_msg))
                        .await
                        .context("failed to update connector status")?;
                    return Err(anyhow::anyhow!("artifact delivery failed: {}", err_msg));
                }
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!("failed to build connector: {}", e));
        }
    }

    // Mark the pending approval as approved.
    let pending: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM approvals WHERE artifact_id = $1 AND status = 'pending'::approval_status LIMIT 1",
    )
    .bind(artifact_id)
    .fetch_optional(&state.pool)
    .await
    .context("failed to find pending approval for artifact")?;

    if let Some(approval_id) = pending {
        approval_repo
            .decide(
                approval_id,
                approver_user_id,
                ApprovalStatus::Approved,
                None,
            )
            .await
            .context("failed to mark approval as approved after delivery")?;
    }

    Ok(())
}

// ── Peer path ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn process_peer(
    state: &AppState,
    routing_id: Uuid,
    workspace_id: Uuid,
    survivor_id: Uuid,
    target_ref: &serde_json::Value,
    signal_title: &str,
    signal_body: &str,
    severity: &str,
) -> anyhow::Result<()> {
    let audit_repo = AuditEventRepo::new(state.pool.clone());
    let peer_repo = PeerRepo::new(state.pool.clone());

    // Resolve the peer_id from target_ref.
    let peer_id_str = target_ref["peer_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("target_ref.peer_id missing for peer routing"))?;
    let peer_id = Uuid::parse_str(peer_id_str).context("target_ref.peer_id is not a valid UUID")?;

    let peer = match peer_repo.get(peer_id).await? {
        Some(p) => p,
        None => {
            warn!(routing_id = %routing_id, peer_id = %peer_id, "peer not found");
            write_peer_audit(
                &audit_repo,
                workspace_id,
                routing_id,
                peer_id,
                "peer_delivery_failed",
                serde_json::json!({ "routing_id": routing_id, "error": "peer not found" }),
            )
            .await?;
            return Ok(());
        }
    };

    let tool_name = "propose_artifact";
    let allowlist = peer_repo.get_tool_allowlist(peer_id).await?;
    if !allowlist.iter().any(|allowed| allowed == tool_name) {
        info!(
            routing_id = %routing_id,
            peer_id = %peer_id,
            tool_name = tool_name,
            "peer_tool_blocked"
        );
        write_peer_audit(
            &audit_repo,
            workspace_id,
            routing_id,
            peer_id,
            "peer_tool_blocked",
            serde_json::json!({
                "routing_id": routing_id,
                "tool": tool_name,
                "reason": "tool not in peer allowlist"
            }),
        )
        .await?;
        return Ok(());
    }

    // Sharing policy enforcement (outbound).
    use crate::services::peer::PolicyDecision;
    match check_sharing_policy(&peer.sharing_policy, severity, workspace_id) {
        PolicyDecision::Blocked(reason) => {
            info!(
                routing_id = %routing_id,
                peer_id = %peer_id,
                reason = %reason,
                "peer_policy_blocked"
            );
            write_peer_audit(
                &audit_repo,
                workspace_id,
                routing_id,
                peer_id,
                "peer_policy_blocked",
                serde_json::json!({ "routing_id": routing_id, "reason": reason }),
            )
            .await?;
            return Ok(());
        }
        PolicyDecision::Allow => {}
    }

    // Find the mcp_client connector for this peer in this workspace.
    let connector_id = match peer_repo
        .find_mcp_connector_for_peer(workspace_id, peer_id)
        .await?
    {
        Some(id) => id,
        None => {
            warn!(
                routing_id = %routing_id,
                peer_id = %peer_id,
                "no mcp connector for peer in workspace — peer_unreachable"
            );
            write_peer_audit(
                &audit_repo,
                workspace_id,
                routing_id,
                peer_id,
                "peer_delivery_failed",
                serde_json::json!({
                    "routing_id": routing_id,
                    "error": "peer_unreachable: no mcp connector in workspace"
                }),
            )
            .await?;
            return Ok(());
        }
    };

    let connector_repo = ConnectorRepo::new(state.pool.clone());
    let connector = connector_repo
        .get(connector_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("connector {} not found", connector_id))?;

    match build_from_row(&connector) {
        Ok(impl_) => {
            // Resolve the peer's own workspace id by calling list_workspaces on it.
            // For simplicity, pass workspace_id = nil and let the peer use its own default.
            let args = serde_json::json!({
                "workspace_id": Uuid::nil().to_string(),
                "kind": "briefing",
                "content": {
                    "title": signal_title,
                    "body": signal_body,
                    "severity": severity,
                },
                "source_survivor_id": survivor_id.to_string(),
            });

            // We need a real workspace_id on the peer. Fetch it via list_workspaces.
            let peer_workspace_id = resolve_peer_workspace_id(&*impl_).await;

            let final_args = serde_json::json!({
                "workspace_id": peer_workspace_id,
                "kind": "briefing",
                "content": {
                    "title": signal_title,
                    "body": signal_body,
                    "severity": severity,
                },
                "source_survivor_id": survivor_id.to_string(),
            });
            let _ = args; // replaced by final_args

            match impl_.invoke("propose_artifact", final_args).await {
                Ok(_) => {
                    info!(
                        routing_id = %routing_id,
                        peer_id = %peer_id,
                        "peer_delivered"
                    );
                    write_peer_audit(
                        &audit_repo,
                        workspace_id,
                        routing_id,
                        peer_id,
                        "peer_delivered",
                        serde_json::json!({ "routing_id": routing_id }),
                    )
                    .await?;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    warn!(
                        routing_id = %routing_id,
                        peer_id = %peer_id,
                        error = %err_msg,
                        "peer_delivery_failed"
                    );
                    write_peer_audit(
                        &audit_repo,
                        workspace_id,
                        routing_id,
                        peer_id,
                        "peer_delivery_failed",
                        serde_json::json!({ "routing_id": routing_id, "error": err_msg }),
                    )
                    .await?;

                    // Mark peer as error if auth/connect failure.
                    if err_msg.contains("auth error") || err_msg.contains("HTTP 4") {
                        peer_repo
                            .update_status(peer_id, PeerStatus::Error)
                            .await
                            .context("failed to update peer status to error")?;
                    }
                }
            }
        }
        Err(e) => {
            let err_msg = format!("failed to build mcp connector: {}", e);
            warn!(routing_id = %routing_id, peer_id = %peer_id, error = %err_msg);
            write_peer_audit(
                &audit_repo,
                workspace_id,
                routing_id,
                peer_id,
                "peer_delivery_failed",
                serde_json::json!({ "routing_id": routing_id, "error": err_msg }),
            )
            .await?;
        }
    }

    Ok(())
}

async fn resolve_peer_workspace_id(impl_: &dyn crate::connectors::ConnectorImpl) -> String {
    // Call list_workspaces on the peer and return the first workspace id.
    // If this fails, fall back to nil UUID — propose_artifact will fail with a useful error.
    match impl_.invoke("list_workspaces", serde_json::json!({})).await {
        Ok(result) => {
            let content_text = result["content"][0]["text"].as_str().unwrap_or("{}");
            let data: serde_json::Value =
                serde_json::from_str(content_text).unwrap_or_else(|_| serde_json::json!({}));
            data["workspaces"][0]["id"]
                .as_str()
                .unwrap_or(&Uuid::nil().to_string())
                .to_string()
        }
        Err(_) => Uuid::nil().to_string(),
    }
}

async fn write_peer_audit(
    audit_repo: &AuditEventRepo,
    workspace_id: Uuid,
    routing_id: Uuid,
    peer_id: Uuid,
    verb: &str,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    audit_repo
        .insert(
            Some(workspace_id),
            ActorKind::System,
            &format!("peer:{}", peer_id),
            verb,
            "peer",
            Some(peer_id),
            payload,
        )
        .await
        .with_context(|| format!("failed to write {} audit event", verb))?;
    let _ = routing_id; // used in payload, keep for clarity
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve a connector from target_ref.connector_id, or fall back to the first
/// active rust_native connector in the workspace.
async fn resolve_connector(
    repo: &ConnectorRepo,
    workspace_id: Uuid,
    target_ref: &serde_json::Value,
) -> anyhow::Result<crate::models::Connector> {
    // Try target_ref.connector_id first.
    if let Some(id_val) = target_ref.get("connector_id") {
        let id_str = id_val
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("target_ref.connector_id is not a string"))?;
        let id = Uuid::parse_str(id_str).context("target_ref.connector_id is not a valid UUID")?;
        if let Some(c) = repo.get(id).await? {
            return Ok(c);
        }
    }

    // Fall back to first active connector in the workspace.
    let connectors = repo.list(workspace_id).await?;
    connectors
        .into_iter()
        .find(|c| c.status == crate::models::ConnectorStatus::Active)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no active connector found in workspace {} for notification delivery",
                workspace_id
            )
        })
}

/// Extract a status code string from an error message for audit payloads.
fn extract_status_code(msg: &str) -> String {
    // Look for patterns like "status 500" or "non-2xx status 500".
    for word in msg.split_whitespace() {
        if let Ok(code) = word
            .trim_matches(|c: char| !c.is_ascii_digit())
            .parse::<u16>()
        {
            if (100..600).contains(&code) {
                return code.to_string();
            }
        }
    }
    "unknown".to_string()
}
