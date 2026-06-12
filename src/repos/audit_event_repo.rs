use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::models::{ActorKind, AuditEvent};

pub struct AuditEventRepo {
    pub(crate) pool: PgPool,
}

#[derive(Debug, Default, Clone)]
pub struct AuditEventFilter {
    pub actor_kind: Option<ActorKind>,
    pub actor_ref: Option<String>,
    pub verbs: Vec<String>,
    pub object_kind: Option<String>,
    pub object_id: Option<Uuid>,
    pub foreign_tenant_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

const AUDIT_EVENT_COLUMNS: &str = "ae.id, ae.workspace_id, ae.actor_kind, ae.actor_ref, ae.verb, \
     ae.object_kind, ae.object_id, ae.payload, ae.created_at, ae.foreign_tenant_id";

/// Shared FROM/WHERE for all filtered audit queries. Org isolation is a
/// DB-layer backstop: the workspaces join requires w.org_id = org_id in
/// addition to the route-layer ensure_workspace_in_org check.
pub(crate) fn push_filtered_from_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    workspace_id: Uuid,
    org_id: Uuid,
    filter: &AuditEventFilter,
) {
    qb.push(" FROM audit_events ae JOIN workspaces w ON w.id = ae.workspace_id AND w.org_id = ");
    qb.push_bind(org_id);
    qb.push(" WHERE ae.workspace_id = ");
    qb.push_bind(workspace_id);
    if let Some(actor_kind) = &filter.actor_kind {
        qb.push(" AND ae.actor_kind = ");
        qb.push_bind(actor_kind.clone());
    }
    if let Some(actor_ref) = &filter.actor_ref {
        qb.push(" AND ae.actor_ref = ");
        qb.push_bind(actor_ref.clone());
    }
    if !filter.verbs.is_empty() {
        qb.push(" AND ae.verb = ANY(");
        qb.push_bind(filter.verbs.clone());
        qb.push(")");
    }
    if let Some(object_kind) = &filter.object_kind {
        qb.push(" AND ae.object_kind = ");
        qb.push_bind(object_kind.clone());
    }
    if let Some(object_id) = filter.object_id {
        qb.push(" AND ae.object_id = ");
        qb.push_bind(object_id);
    }
    if let Some(foreign_tenant_id) = &filter.foreign_tenant_id {
        qb.push(" AND ae.foreign_tenant_id = ");
        qb.push_bind(foreign_tenant_id.clone());
    }
    if let Some(since) = filter.since {
        qb.push(" AND ae.created_at >= ");
        qb.push_bind(since);
    }
    if let Some(until) = filter.until {
        qb.push(" AND ae.created_at < ");
        qb.push_bind(until);
    }
}

fn push_cursor_predicate(qb: &mut QueryBuilder<'_, Postgres>, cursor: (DateTime<Utc>, Uuid)) {
    qb.push(" AND (ae.created_at, ae.id) < (");
    qb.push_bind(cursor.0);
    qb.push(", ");
    qb.push_bind(cursor.1);
    qb.push(")");
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

    /// Keyset page: WHERE (created_at, id) < cursor ORDER BY created_at DESC, id DESC.
    /// All filter values are bind parameters; `limit` is pre-clamped 1..=200 by the route.
    pub async fn list_filtered(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &AuditEventFilter,
        cursor: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        let mut qb = QueryBuilder::new(format!("SELECT {AUDIT_EVENT_COLUMNS}"));
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        if let Some(cursor) = cursor {
            push_cursor_predicate(&mut qb, cursor);
        }
        qb.push(" ORDER BY ae.created_at DESC, ae.id DESC LIMIT ");
        qb.push_bind(limit);
        qb.build_query_as::<AuditEvent>()
            .fetch_all(&self.pool)
            .await
            .context("failed to list filtered audit_events")
    }

    /// Export key query (index-only, cheap): the page of (created_at, id)
    /// keys that decides both truncation and the continuation cursor.
    pub async fn keyset_page(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &AuditEventFilter,
        cursor: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<(DateTime<Utc>, Uuid)>> {
        let mut qb = QueryBuilder::new("SELECT ae.created_at, ae.id");
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        if let Some(cursor) = cursor {
            push_cursor_predicate(&mut qb, cursor);
        }
        qb.push(" ORDER BY ae.created_at DESC, ae.id DESC LIMIT ");
        qb.push_bind(limit);
        let rows = qb
            .build_query_as::<(DateTime<Utc>, Uuid)>()
            .fetch_all(&self.pool)
            .await
            .context("failed to fetch audit_events key page")?;
        Ok(rows)
    }

    /// Export row stream: same WHERE as `list_filtered`, bounded inclusively
    /// by the first/last keys returned from `keyset_page`. The window is
    /// frozen by those bounds, so this sees exactly the key query's rows.
    pub fn stream_between_keys(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &AuditEventFilter,
        first: (DateTime<Utc>, Uuid),
        last: (DateTime<Utc>, Uuid),
    ) -> impl futures_util::Stream<Item = sqlx::Result<AuditEvent>> + Send + 'static {
        let pool = self.pool.clone();
        let mut qb: QueryBuilder<'static, Postgres> =
            QueryBuilder::new(format!("SELECT {AUDIT_EVENT_COLUMNS}"));
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(" AND (ae.created_at, ae.id) <= (");
        qb.push_bind(first.0);
        qb.push(", ");
        qb.push_bind(first.1);
        qb.push(")");
        qb.push(" AND (ae.created_at, ae.id) >= (");
        qb.push_bind(last.0);
        qb.push(", ");
        qb.push_bind(last.1);
        qb.push(")");
        qb.push(" ORDER BY ae.created_at DESC, ae.id DESC");
        async_stream::stream! {
            use futures_util::StreamExt;
            let mut qb = qb;
            let mut rows = qb.build_query_as::<AuditEvent>().fetch(&pool);
            while let Some(row) = rows.next().await {
                yield row;
            }
        }
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
