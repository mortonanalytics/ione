use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::models::{outcome, ActorKind, InteractionEvent};

pub struct InteractionEventRepo {
    pool: PgPool,
}

#[derive(Debug, Default, Clone)]
pub struct InteractionEventFilter {
    pub peer_id: Option<Uuid>,
    pub caller_user_id: Option<Uuid>,
    pub caller_peer_id: Option<Uuid>,
    pub caller_token_id: Option<Uuid>,
    pub outcome: Option<String>,
    pub session_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeCount {
    pub outcome: String,
    pub count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalCount {
    pub caller_kind: ActorKind,
    pub caller_id: Option<Uuid>,
    pub count: i64,
    pub deny_count: i64,
    pub error_count: i64,
}

const INTERACTION_COLUMNS: &str = "ie.id, ie.org_id, ie.workspace_id, ie.peer_id, ie.peer_name, \
     ie.tool_name, ie.caller_kind, ie.caller_user_id, ie.caller_peer_id, ie.caller_token_id, \
     ie.session_id, ie.sequence_number, ie.outcome, ie.latency_ms, ie.detail, ie.recorded_at";

pub(crate) fn push_filtered_from_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    workspace_id: Uuid,
    org_id: Uuid,
    filter: &InteractionEventFilter,
) {
    qb.push(
        " FROM interaction_events ie JOIN workspaces w ON w.id = ie.workspace_id AND w.org_id = ",
    );
    qb.push_bind(org_id);
    qb.push(" WHERE ie.workspace_id = ");
    qb.push_bind(workspace_id);
    qb.push(" AND ie.org_id = ");
    qb.push_bind(org_id);

    if let Some(peer_id) = filter.peer_id {
        qb.push(" AND ie.peer_id = ");
        qb.push_bind(peer_id);
    }
    if let Some(caller_user_id) = filter.caller_user_id {
        qb.push(" AND ie.caller_user_id = ");
        qb.push_bind(caller_user_id);
    }
    if let Some(caller_peer_id) = filter.caller_peer_id {
        qb.push(" AND ie.caller_peer_id = ");
        qb.push_bind(caller_peer_id);
    }
    if let Some(caller_token_id) = filter.caller_token_id {
        qb.push(" AND ie.caller_token_id = ");
        qb.push_bind(caller_token_id);
    }
    if let Some(outcome) = &filter.outcome {
        qb.push(" AND ie.outcome = ");
        qb.push_bind(outcome.clone());
    }
    if let Some(session_id) = filter.session_id {
        qb.push(" AND ie.session_id = ");
        qb.push_bind(session_id);
    }
    if let Some(since) = filter.since {
        qb.push(" AND ie.recorded_at >= ");
        qb.push_bind(since);
    }
    if let Some(until) = filter.until {
        qb.push(" AND ie.recorded_at < ");
        qb.push_bind(until);
    }
}

fn push_cursor_predicate(qb: &mut QueryBuilder<'_, Postgres>, cursor: (DateTime<Utc>, Uuid)) {
    qb.push(" AND (ie.recorded_at, ie.id) < (");
    qb.push_bind(cursor.0);
    qb.push(", ");
    qb.push_bind(cursor.1);
    qb.push(")");
}

pub fn sanitize_detail(mut detail: Value) -> Value {
    crate::util::redact::scrub_error_fields(&mut detail);
    if detail.to_string().len() <= 4096 {
        return detail;
    }
    json!({ "truncated": true })
}

impl InteractionEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_batch(&self, events: &[InteractionEvent]) -> anyhow::Result<u64> {
        if events.is_empty() {
            return Ok(0);
        }

        let mut qb = QueryBuilder::new(
            "INSERT INTO interaction_events
             (id, org_id, workspace_id, peer_id, peer_name, tool_name, caller_kind,
              caller_user_id, caller_peer_id, caller_token_id, session_id, sequence_number,
              outcome, latency_ms, detail, recorded_at) ",
        );
        qb.push_values(events, |mut b, event| {
            b.push_bind(event.id)
                .push_bind(event.org_id)
                .push_bind(event.workspace_id)
                .push_bind(event.peer_id)
                .push_bind(event.peer_name.clone())
                .push_bind(event.tool_name.clone())
                .push_bind(event.caller_kind.clone())
                .push_bind(event.caller_user_id)
                .push_bind(event.caller_peer_id)
                .push_bind(event.caller_token_id)
                .push_bind(event.session_id)
                .push_bind(event.sequence_number)
                .push_bind(event.outcome.clone())
                .push_bind(event.latency_ms)
                .push_bind(sanitize_detail(event.detail.clone()))
                .push_bind(event.recorded_at);
        });

        qb.build()
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .context("failed to insert interaction_events batch")
    }

    pub async fn list_filtered(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &InteractionEventFilter,
        cursor: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<InteractionEvent>> {
        let mut qb = QueryBuilder::new(format!("SELECT {INTERACTION_COLUMNS}"));
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        if let Some(cursor) = cursor {
            push_cursor_predicate(&mut qb, cursor);
        }
        qb.push(" ORDER BY ie.recorded_at DESC, ie.id DESC LIMIT ");
        qb.push_bind(limit);
        qb.build_query_as::<InteractionEvent>()
            .fetch_all(&self.pool)
            .await
            .context("failed to list filtered interaction_events")
    }

    pub async fn list_session_steps(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        session_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<InteractionEvent>> {
        let filter = InteractionEventFilter {
            session_id: Some(session_id),
            ..Default::default()
        };
        let mut qb = QueryBuilder::new(format!("SELECT {INTERACTION_COLUMNS}"));
        push_filtered_from_where(&mut qb, workspace_id, org_id, &filter);
        qb.push(
            " ORDER BY ie.sequence_number ASC NULLS LAST, ie.recorded_at ASC, ie.id ASC LIMIT ",
        );
        qb.push_bind(limit);
        qb.build_query_as::<InteractionEvent>()
            .fetch_all(&self.pool)
            .await
            .context("failed to list interaction session steps")
    }

    pub async fn outcome_summary(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &InteractionEventFilter,
    ) -> anyhow::Result<Vec<OutcomeCount>> {
        let mut qb = QueryBuilder::new("SELECT ie.outcome, COUNT(*)::bigint AS count");
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(" GROUP BY ie.outcome ORDER BY ie.outcome ASC");
        qb.build_query_as::<OutcomeCount>()
            .fetch_all(&self.pool)
            .await
            .context("failed to summarize interaction outcomes")
    }

    pub async fn count_by_bucket(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        bucket: &str,
        filter: &InteractionEventFilter,
    ) -> anyhow::Result<Vec<Value>> {
        let bucket_expr = match bucket {
            "minute" => "minute",
            "hour" => "hour",
            "day" => "day",
            "week" => "week",
            _ => anyhow::bail!("unsupported interaction bucket"),
        };
        let mut qb = QueryBuilder::new(format!(
            "SELECT date_trunc('{bucket_expr}', ie.recorded_at) AS bucket, \
                    ie.peer_id, ie.peer_name, ie.outcome, COUNT(*)::bigint AS count"
        ));
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(
            " GROUP BY bucket, ie.peer_id, ie.peer_name, ie.outcome \
                 ORDER BY bucket ASC, ie.peer_name ASC, ie.outcome ASC",
        );
        let rows = qb
            .build_query_as::<(DateTime<Utc>, Uuid, String, String, i64)>()
            .fetch_all(&self.pool)
            .await
            .context("failed to count interaction events by bucket")?;
        Ok(rows
            .into_iter()
            .map(|(bucket, peer_id, peer_name, outcome, count)| {
                json!({
                    "bucket": bucket,
                    "peerId": peer_id,
                    "peerName": peer_name,
                    "outcome": outcome,
                    "count": count,
                })
            })
            .collect())
    }

    pub async fn count_by_principal(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        filter: &InteractionEventFilter,
    ) -> anyhow::Result<Vec<PrincipalCount>> {
        let mut qb = QueryBuilder::new(
            "SELECT ie.caller_kind,
                    COALESCE(ie.caller_user_id, ie.caller_peer_id, ie.caller_token_id) AS caller_id,
                    COUNT(*)::bigint AS count,
                    COUNT(*) FILTER (WHERE ie.outcome = 'deny')::bigint AS deny_count,
                    COUNT(*) FILTER (WHERE ie.outcome = 'error')::bigint AS error_count",
        );
        push_filtered_from_where(&mut qb, workspace_id, org_id, filter);
        qb.push(" GROUP BY ie.caller_kind, caller_id ORDER BY count DESC, caller_id ASC LIMIT 200");
        qb.build_query_as::<PrincipalCount>()
            .fetch_all(&self.pool)
            .await
            .context("failed to count interaction events by principal")
    }

    pub async fn validate_filter(filter: &InteractionEventFilter) -> anyhow::Result<()> {
        if let Some(outcome) = &filter.outcome {
            if !outcome::is_valid(outcome) {
                anyhow::bail!("outcome must be one of allow, deny, pending, error");
            }
        }
        Ok(())
    }
}
