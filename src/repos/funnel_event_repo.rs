use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::models::FunnelEventInput;

pub struct FunnelEventRepo {
    pool: PgPool,
}

impl FunnelEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn append(&self, input: FunnelEventInput) -> Result<()> {
        sqlx::query(
            "INSERT INTO funnel_events (user_id, session_id, workspace_id, event_kind, detail)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(input.user_id)
        .bind(input.session_id)
        .bind(input.workspace_id)
        .bind(input.event_kind)
        .bind(input.detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn counts_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT event_kind, count(*)::bigint
               FROM funnel_events
              WHERE occurred_at BETWEEN $1 AND $2
              GROUP BY event_kind
              ORDER BY count(*) DESC",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
