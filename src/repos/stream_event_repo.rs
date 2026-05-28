use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::StreamEvent;
use crate::services::event_layers::{GeoEventRow, GeoStreamRow};

pub struct StreamEventRepo {
    pub(crate) pool: PgPool,
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

    /// Fetch the catalog of geo-mapped streams in a workspace (Q1) and the
    /// events for those streams within the window (Q2, `LIMIT + 1` for
    /// unambiguous truncation detection). Both queries are org-scoped via the
    /// same workspace∈org fence used by `/map-layers`.
    pub async fn fetch_geo_events(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Option<Uuid>,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<(Vec<GeoStreamRow>, Vec<GeoEventRow>)> {
        let catalog = sqlx::query_as::<_, GeoStreamRow>(
            "SELECT s.id AS stream_id, s.name AS stream_name, s.view_config
             FROM streams s
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
               AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
               AND s.view_config IS NOT NULL
               AND ($3::uuid IS NULL OR s.id = $3)
             ORDER BY s.name",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(stream_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch geo-mapped stream catalog")?;

        let events = sqlx::query_as::<_, GeoEventRow>(
            "SELECT se.id AS event_id, se.stream_id, se.payload, se.observed_at
             FROM stream_events se
             JOIN streams s    ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
               AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
               AND s.view_config IS NOT NULL
               AND ($3::uuid IS NULL OR s.id = $3)
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             ORDER BY se.observed_at DESC
             LIMIT $6 + 1",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(stream_id)
        .bind(since)
        .bind(until)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch geo events")?;

        Ok((catalog, events))
    }

    pub async fn latest_observed_at(
        &self,
        stream_id: Uuid,
    ) -> anyhow::Result<Option<DateTime<Utc>>> {
        sqlx::query_scalar(
            "SELECT observed_at
             FROM stream_events
             WHERE stream_id = $1
             ORDER BY observed_at DESC
             LIMIT 1",
        )
        .bind(stream_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch latest observed_at for stream")
    }
}
