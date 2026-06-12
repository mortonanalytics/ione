use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// Recovery-gap aggregation over `pipeline_events`.
pub struct PipelineEventAggregateRepo {
    pool: PgPool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct RecoveryGapRow {
    pub connector_id: Uuid,
    pub gap_seconds: f64,
    pub from_stage: String,
    pub occurred_at: DateTime<Utc>,
}

impl PipelineEventAggregateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// For each fault event (`stall`/`error`), the recovery point is the
    /// earliest later `publish_started` on the same connector, ignoring any
    /// intervening stages (`first_event`, repeated faults, other streams).
    /// Plain LEAD() (next-event-of-any-stage) would be wrong — lateral MIN.
    /// NULL-connector fault rows are deliberately excluded.
    pub async fn recovery_gaps(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        connector_id: Option<Uuid>,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> anyhow::Result<Vec<RecoveryGapRow>> {
        sqlx::query_as::<_, RecoveryGapRow>(
            "SELECT f.connector_id, f.stage AS from_stage, f.occurred_at,
                    EXTRACT(EPOCH FROM (r.recovered_at - f.occurred_at))::double precision AS gap_seconds
             FROM pipeline_events f
             JOIN workspaces w ON w.id = f.workspace_id AND w.org_id = $2
             JOIN LATERAL (
               SELECT MIN(occurred_at) AS recovered_at FROM pipeline_events r
               WHERE r.workspace_id = f.workspace_id AND r.connector_id = f.connector_id
                 AND r.stage = 'publish_started' AND r.occurred_at > f.occurred_at
             ) r ON r.recovered_at IS NOT NULL
             WHERE f.workspace_id = $1 AND f.stage IN ('stall','error')
               AND f.connector_id IS NOT NULL
               AND ($3::uuid IS NULL OR f.connector_id = $3)
               AND f.occurred_at >= $4 AND f.occurred_at < $5
             ORDER BY f.occurred_at
             LIMIT 10000",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(connector_id)
        .bind(since)
        .bind(until)
        .fetch_all(&self.pool)
        .await
        .context("failed to compute recovery gaps")
    }
}
