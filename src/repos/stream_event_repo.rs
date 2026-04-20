use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::StreamEvent;

pub struct StreamEventRepo {
    pub pool: PgPool,
}

impl StreamEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a stream event only if no row with the same (stream_id, observed_at) exists.
    /// Returns true if a row was inserted, false if it was a duplicate.
    pub async fn insert_if_absent(
        &self,
        stream_id: Uuid,
        payload: serde_json::Value,
        observed_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let rows_affected = sqlx::query(
            "INSERT INTO stream_events (stream_id, payload, observed_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (stream_id, observed_at) DO NOTHING",
        )
        .bind(stream_id)
        .bind(payload)
        .bind(observed_at)
        .execute(&self.pool)
        .await
        .context("failed to insert stream event")?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    pub async fn list_recent(
        &self,
        stream_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<StreamEvent>> {
        sqlx::query_as::<_, StreamEvent>(
            "SELECT id, stream_id, payload, observed_at, ingested_at, embedding
             FROM stream_events
             WHERE stream_id = $1
             ORDER BY observed_at DESC
             LIMIT $2",
        )
        .bind(stream_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list recent stream events")
    }
}
