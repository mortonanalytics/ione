use anyhow::Context;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    models::{Severity, SignalSource},
    repos::WebhookEventRepo,
    routes::webhooks::WebhookEnvelope,
    state::AppState,
};

pub enum IngestOutcome {
    Created(Vec<Uuid>),
    Duplicate,
    NoBinding,
}

pub async fn ingest_webhook_event(
    state: &AppState,
    peer_id: Uuid,
    env: &WebhookEnvelope,
) -> anyhow::Result<IngestOutcome> {
    ingest_with_pool(&state.pool, peer_id, env).await
}

async fn ingest_with_pool(
    pool: &PgPool,
    peer_id: Uuid,
    env: &WebhookEnvelope,
) -> anyhow::Result<IngestOutcome> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin webhook ingest transaction")?;

    let workspace_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT b.workspace_id
         FROM workspace_peer_bindings b
         JOIN workspaces w ON w.id = b.workspace_id
         JOIN peers p ON p.id = b.peer_id
         WHERE b.peer_id = $1
           AND b.foreign_tenant_id = $2
           AND b.status = 'active'::binding_status
           AND w.closed_at IS NULL
           AND p.org_id = w.org_id
         ORDER BY b.created_at ASC",
    )
    .bind(peer_id)
    .bind(&env.foreign_tenant_id)
    .fetch_all(&mut *tx)
    .await
    .context("failed to resolve webhook workspace bindings")?;

    if workspace_ids.is_empty() {
        tx.rollback().await.ok();
        return Ok(IngestOutcome::NoBinding);
    }

    let inserted = WebhookEventRepo::try_insert_seen_tx(&mut tx, &env.id, peer_id).await?;
    if !inserted {
        tx.rollback().await.ok();
        return Ok(IngestOutcome::Duplicate);
    }

    let severity = map_severity(&env.severity);
    let approval_required =
        env.approval_required || matches!(severity, Severity::Flagged | Severity::Command);
    let evidence = serde_json::json!({
        "peer_id": peer_id,
        "event_id": env.id,
        "occurred_at": env.occurred_at,
        "type": env.r#type,
        "foreign_tenant_id": env.foreign_tenant_id,
        "data": env.data,
    });

    let mut signal_ids = Vec::with_capacity(workspace_ids.len());
    for workspace_id in workspace_ids {
        let row = sqlx::query(
            "INSERT INTO signals
               (workspace_id, source, title, body, evidence, severity, generator_model, approval_required)
             VALUES ($1, $2, $3, $4, $5, $6, NULL, $7)
             RETURNING id",
        )
        .bind(workspace_id)
        .bind(SignalSource::ConnectorEvent)
        .bind(&env.r#type)
        .bind(webhook_signal_body(env))
        .bind(&evidence)
        .bind(severity.clone())
        .bind(approval_required)
        .fetch_one(&mut *tx)
        .await
        .context("failed to insert webhook signal")?;
        signal_ids.push(row.get("id"));
    }

    tx.commit()
        .await
        .context("failed to commit webhook ingest transaction")?;
    Ok(IngestOutcome::Created(signal_ids))
}

pub fn map_severity(s: &Option<String>) -> Severity {
    match s.as_deref().map(str::to_lowercase).as_deref() {
        Some("command") => Severity::Command,
        Some("flagged") => Severity::Flagged,
        _ => Severity::Routine,
    }
}

fn webhook_signal_body(env: &WebhookEnvelope) -> String {
    format!(
        "Webhook event {} occurred at {} for foreign tenant {}.",
        env.id, env.occurred_at, env.foreign_tenant_id
    )
}
