use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{ActorKind, AuditEvent};

pub struct AuditEventRepo {
    pub(crate) pool: PgPool,
}

impl AuditEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        &self,
        workspace_id: Option<Uuid>,
        actor_kind: ActorKind,
        actor_ref: &str,
        verb: &str,
        object_kind: &str,
        object_id: Option<Uuid>,
        payload: serde_json::Value,
    ) -> anyhow::Result<AuditEvent> {
        self.insert_with_foreign_tenant(
            workspace_id,
            actor_kind,
            actor_ref,
            verb,
            object_kind,
            object_id,
            payload,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_with_foreign_tenant(
        &self,
        workspace_id: Option<Uuid>,
        actor_kind: ActorKind,
        actor_ref: &str,
        verb: &str,
        object_kind: &str,
        object_id: Option<Uuid>,
        mut payload: serde_json::Value,
        foreign_tenant_id: Option<&str>,
    ) -> anyhow::Result<AuditEvent> {
        crate::util::redact::scrub_error_fields(&mut payload);
        sqlx::query_as::<_, AuditEvent>(
            "INSERT INTO audit_events
               (workspace_id, actor_kind, actor_ref, verb, object_kind, object_id, payload, foreign_tenant_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, workspace_id, actor_kind, actor_ref, verb, object_kind, object_id,
                       payload, created_at, foreign_tenant_id",
        )
        .bind(workspace_id)
        .bind(actor_kind)
        .bind(actor_ref)
        .bind(verb)
        .bind(object_kind)
        .bind(object_id)
        .bind(payload)
        .bind(foreign_tenant_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert audit_event")
    }

    pub async fn list_for_workspace(
        &self,
        workspace_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        sqlx::query_as::<_, AuditEvent>(
            "SELECT id, workspace_id, actor_kind, actor_ref, verb, object_kind, object_id,
                    payload, created_at, foreign_tenant_id
             FROM audit_events
             WHERE workspace_id = $1
             ORDER BY created_at DESC
             LIMIT $2",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list audit_events for workspace")
    }
}
