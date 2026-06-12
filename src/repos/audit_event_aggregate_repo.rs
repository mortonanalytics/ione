use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, QueryBuilder, Row};
use uuid::Uuid;

use crate::repos::audit_event_repo::{push_filtered_from_where, AuditEventFilter};

/// Aggregates over `audit_events`. Mirrors `StreamEventAggregateRepo`'s
/// interface shape but is deliberately not generalized from it — the scope
/// joins differ (stream events join 3 hops; audit events join workspaces only).
pub struct AuditEventAggregateRepo {
    pool: PgPool,
}

/// Allow-listed GROUP BY dimension, mapped to a hard-coded column ident —
/// never interpolated from raw user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCol {
    ActorKind,
    Verb,
    ActorRef,
}

impl GroupCol {
    fn key_expr(self) -> &'static str {
        match self {
            GroupCol::ActorKind => "ae.actor_kind::text",
            GroupCol::Verb => "ae.verb",
            GroupCol::ActorRef => "ae.actor_ref",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BucketCountRow {
    pub key: String,
    pub bucket_start: DateTime<Utc>,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct ActorCountRow {
    pub key: String,
    pub count: i64,
}

/// `bucket` must be pre-validated by the route against {minute,hour,day,week};
/// it is interpolated into date_trunc. Fail loudly rather than silently default.
fn bucket_expr(bucket: &str) -> String {
    match bucket {
        "minute" | "hour" | "day" | "week" => format!("date_trunc('{bucket}', ae.created_at)"),
        other => panic!("bucket '{other}' must be validated before bucket_expr"),
    }
}

impl AuditEventAggregateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn count_by_bucket(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        bucket: &str,
        group_col: GroupCol,
        filter: &AuditEventFilter,
    ) -> anyhow::Result<Vec<BucketCountRow>> {
        let bucket_expr = bucket_expr(bucket);
        let key_expr = group_col.key_expr();
        let mut qb = QueryBuilder::new(format!(
            "SELECT {bucket_expr} AS bucket_start, {key_expr} AS key, COUNT(*)::bigint AS count"
        ));
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(" GROUP BY bucket_start, key ORDER BY bucket_start, key");
        let rows = qb
            .build()
            .fetch_all(&self.pool)
            .await
            .context("failed to aggregate audit counts by bucket")?;
        Ok(rows
            .into_iter()
            .map(|row| BucketCountRow {
                key: row.get("key"),
                bucket_start: row.get("bucket_start"),
                count: row.get("count"),
            })
            .collect())
    }

    pub async fn count_by_actor(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &AuditEventFilter,
    ) -> anyhow::Result<Vec<ActorCountRow>> {
        let mut qb = QueryBuilder::new("SELECT ae.actor_ref AS key, COUNT(*)::bigint AS count");
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(" GROUP BY key ORDER BY count DESC, key ASC LIMIT 200");
        let rows = qb
            .build()
            .fetch_all(&self.pool)
            .await
            .context("failed to aggregate audit counts by actor")?;
        Ok(rows
            .into_iter()
            .map(|row| ActorCountRow {
                key: row.get("key"),
                count: row.get("count"),
            })
            .collect())
    }
}
