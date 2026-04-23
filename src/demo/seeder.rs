/// Demo workspace seeder.
///
/// `seed_demo_if_enabled(pool)` — gated on `IONE_SEED_DEMO=1`; idempotent.
/// `purge_demo(pool)` — removes the demo workspace and its audit events atomically.
use anyhow::Context;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::demo::{fixture as f, DEMO_WORKSPACE_ID};
use crate::models::{ActorKind, ArtifactKind, MessageRole};

/// Seed the demo workspace if `IONE_SEED_DEMO=1`.
/// No-op if the workspace already exists (re-entrant).
pub async fn seed_demo_if_enabled(pool: &PgPool) -> anyhow::Result<()> {
    if std::env::var("IONE_SEED_DEMO").as_deref() != Ok("1") {
        return Ok(());
    }

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1)",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_one(pool)
    .await
    .context("failed to check demo workspace existence")?;

    if exists {
        return Ok(());
    }

    crate::repos::bootstrap::ensure_default_org_and_user(pool)
        .await
        .context("demo seeder failed to ensure default org+user bootstrap")?;

    seed_demo(pool).await
}

async fn seed_demo(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin demo seed transaction")?;

    let (org_id, user_id) = resolve_default_org_and_user(&mut tx).await?;
    seed_workspace(&mut tx, org_id).await?;
    seed_roles(&mut tx).await?;
    seed_connectors(&mut tx).await?;
    seed_streams(&mut tx).await?;
    seed_stream_events(&mut tx).await?;
    seed_signals(&mut tx).await?;
    seed_survivors(&mut tx).await?;
    seed_routing_decisions(&mut tx).await?;
    seed_artifacts(&mut tx).await?;
    seed_approvals(&mut tx, user_id).await?;
    seed_audit_events(&mut tx).await?;
    seed_conversation(&mut tx, user_id).await?;

    tx.commit()
        .await
        .context("failed to commit demo seed transaction")?;

    tracing::info!("demo workspace seeded successfully");
    Ok(())
}

/// Purge the demo workspace and its audit events atomically.
/// The `audit_events.workspace_id` FK is `ON DELETE SET NULL`, so we must
/// delete those rows explicitly before deleting the workspace.
pub async fn purge_demo(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin purge transaction")?;

    sqlx::query("DELETE FROM audit_events WHERE workspace_id = $1")
        .bind(DEMO_WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .context("failed to delete demo audit_events")?;

    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(DEMO_WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .context("failed to delete demo workspace")?;

    tx.commit()
        .await
        .context("failed to commit purge transaction")?;

    tracing::info!("demo workspace purged");
    Ok(())
}

// ─── Private seed helpers ─────────────────────────────────────────────────────

async fn resolve_default_org_and_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<(Uuid, Uuid)> {
    let org_id: Uuid =
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
            .fetch_optional(&mut **tx)
            .await
            .context("failed to query Default Org")?
            .context("Default Org does not exist — run app bootstrap first")?;

    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE org_id = $1 LIMIT 1")
            .bind(org_id)
            .fetch_optional(&mut **tx)
            .await
            .context("failed to query default user")?
            .context("no users found in Default Org — run app bootstrap first")?;

    Ok((org_id, user_id))
}

async fn seed_workspace(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO workspaces (id, org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'IONe Demo Ops', 'fire-ops', 'continuous')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(DEMO_WORKSPACE_ID)
    .bind(org_id)
    .execute(&mut **tx)
    .await
    .context("failed to insert demo workspace")?;
    Ok(())
}

async fn seed_roles(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> anyhow::Result<()> {
    let roles = [
        (f::ROLE_IC, "incident_commander", 3i32),
        (f::ROLE_FIELD_LEAD, "field_lead", 2i32),
        (f::ROLE_ANALYST, "analyst", 1i32),
    ];
    for (id, name, coc_level) in roles {
        sqlx::query(
            "INSERT INTO roles (id, workspace_id, name, coc_level)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workspace_id, name, coc_level) DO NOTHING",
        )
        .bind(id)
        .bind(DEMO_WORKSPACE_ID)
        .bind(name)
        .bind(coc_level)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo role")?;
    }
    Ok(())
}

async fn seed_connectors(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for c in f::connectors() {
        sqlx::query(
            "INSERT INTO connectors (id, workspace_id, kind, name, config)
             VALUES ($1, $2, 'rust_native'::connector_kind, $3, $4)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(c.id)
        .bind(DEMO_WORKSPACE_ID)
        .bind(c.name)
        .bind(c.config)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo connector")?;
    }
    Ok(())
}

async fn seed_streams(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> anyhow::Result<()> {
    for c in f::connectors() {
        sqlx::query(
            "INSERT INTO streams (id, connector_id, name, schema)
             VALUES ($1, $2, 'default', '{}'::jsonb)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(c.stream_id)
        .bind(c.id)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo stream")?;
    }
    Ok(())
}

async fn seed_stream_events(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for ev in f::stream_events() {
        let observed_at = Utc::now() - Duration::minutes(ev.offset_minutes);
        sqlx::query(
            "INSERT INTO stream_events (stream_id, payload, observed_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (stream_id, observed_at) DO NOTHING",
        )
        .bind(ev.stream_id)
        .bind(ev.payload)
        .bind(observed_at)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo stream event")?;
    }
    Ok(())
}

async fn seed_signals(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for s in f::signals() {
        sqlx::query(
            "INSERT INTO signals (id, workspace_id, source, title, body, evidence, severity)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(s.id)
        .bind(DEMO_WORKSPACE_ID)
        .bind(s.source)
        .bind(s.title)
        .bind(s.body)
        .bind(s.evidence)
        .bind(s.severity)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo signal")?;
    }
    Ok(())
}

async fn seed_survivors(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for sv in f::survivors() {
        sqlx::query(
            "INSERT INTO survivors
               (id, signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
             VALUES ($1, $2, 'phi4-reasoning:14b', $3, $4, $5, '[]'::jsonb)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(sv.id)
        .bind(sv.signal_id)
        .bind(sv.verdict)
        .bind(sv.rationale)
        .bind(sv.confidence)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo survivor")?;
    }
    Ok(())
}

async fn seed_routing_decisions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for rd in f::routing_decisions() {
        sqlx::query(
            "INSERT INTO routing_decisions
               (id, survivor_id, target_kind, target_ref, classifier_model, rationale)
             VALUES ($1, $2, $3, $4, 'demo-router', 'demo seed routing')
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(rd.id)
        .bind(rd.survivor_id)
        .bind(rd.target_kind)
        .bind(rd.target_ref)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo routing_decision")?;
    }
    Ok(())
}

async fn seed_artifacts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for art in f::artifacts() {
        let kind_str = artifact_kind_str(art.kind);
        sqlx::query(
            "INSERT INTO artifacts (id, workspace_id, kind, source_survivor_id, content)
             VALUES ($1, $2, $3::artifact_kind, $4, $5)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(art.id)
        .bind(DEMO_WORKSPACE_ID)
        .bind(kind_str)
        .bind(art.survivor_id)
        .bind(art.content)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo artifact")?;
    }
    Ok(())
}

fn artifact_kind_str(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Briefing => "briefing",
        ArtifactKind::NotificationDraft => "notification_draft",
        ArtifactKind::ResourceOrder => "resource_order",
        ArtifactKind::Message => "message",
        ArtifactKind::Report => "report",
    }
}

async fn seed_approvals(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> anyhow::Result<()> {
    for ap in f::approvals() {
        match ap.decided {
            None => {
                sqlx::query(
                    "INSERT INTO approvals (id, artifact_id, status)
                     VALUES ($1, $2, 'pending'::approval_status)
                     ON CONFLICT (id) DO NOTHING",
                )
                .bind(ap.id)
                .bind(ap.artifact_id)
                .execute(&mut **tx)
                .await
                .context("failed to insert pending approval")?;
            }
            Some(approved) => {
                let status = if approved { "approved" } else { "rejected" };
                sqlx::query(
                    "INSERT INTO approvals
                       (id, artifact_id, approver_user_id, status, comment, decided_at)
                     VALUES ($1, $2, $3, $4::approval_status, $5, now())
                     ON CONFLICT (id) DO NOTHING",
                )
                .bind(ap.id)
                .bind(ap.artifact_id)
                .bind(user_id)
                .bind(status)
                .bind(ap.comment)
                .execute(&mut **tx)
                .await
                .context("failed to insert decided approval")?;
            }
        }
    }
    Ok(())
}

async fn seed_audit_events(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    for ev in f::audit_events() {
        let actor_kind_str = actor_kind_str(ev.actor_kind);
        sqlx::query(
            "INSERT INTO audit_events
               (workspace_id, actor_kind, actor_ref, verb, object_kind, object_id, payload)
             VALUES ($1, $2::actor_kind, $3, $4, $5, $6, '{}'::jsonb)",
        )
        .bind(ev.workspace_id)
        .bind(actor_kind_str)
        .bind(ev.actor_ref)
        .bind(ev.verb)
        .bind(ev.object_kind)
        .bind(ev.object_id)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo audit_event")?;
    }
    Ok(())
}

fn actor_kind_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::User => "user",
        ActorKind::System => "system",
        ActorKind::Peer => "peer",
    }
}

async fn seed_conversation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO conversations (id, user_id, workspace_id, title)
         VALUES ($1, $2, $3, 'IONe Demo — Generator & Critic Exchange')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(f::CONV_1)
    .bind(user_id)
    .bind(DEMO_WORKSPACE_ID)
    .execute(&mut **tx)
    .await
    .context("failed to insert demo conversation")?;

    for msg in f::conversation_messages() {
        let is_assistant = msg.role == MessageRole::Assistant;
        let role_str = message_role_str(msg.role);
        let model = if is_assistant { Some("canned") } else { None };
        sqlx::query(
            "INSERT INTO messages (conversation_id, role, content, model)
             VALUES ($1, $2::message_role, $3, $4)",
        )
        .bind(f::CONV_1)
        .bind(role_str)
        .bind(msg.content)
        .bind(model)
        .execute(&mut **tx)
        .await
        .context("failed to insert demo message")?;
    }
    Ok(())
}

fn message_role_str(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}
