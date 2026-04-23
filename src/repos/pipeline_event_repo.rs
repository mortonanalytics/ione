use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{PipelineEvent, PipelineEventInput, PipelineEventStage};

pub struct PipelineEventRepo {
    pub pool: PgPool,
}

pub struct EventFilter {
    pub connector_id: Option<Uuid>,
    pub stage: Option<PipelineEventStage>,
    pub limit: i64,
    pub before: Option<DateTime<Utc>>,
}

impl PipelineEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn append(&self, input: PipelineEventInput) -> anyhow::Result<PipelineEvent> {
        sqlx::query_as::<_, PipelineEvent>(
            "INSERT INTO pipeline_events (workspace_id, connector_id, stream_id, stage, detail)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, workspace_id, connector_id, stream_id, stage, detail, occurred_at",
        )
        .bind(input.workspace_id)
        .bind(input.connector_id)
        .bind(input.stream_id)
        .bind(input.stage.as_str())
        .bind(input.detail)
        .fetch_one(&self.pool)
        .await
        .context("failed to append pipeline event")
    }

    pub async fn list(
        &self,
        workspace_id: Uuid,
        filter: EventFilter,
    ) -> anyhow::Result<(Vec<PipelineEvent>, Option<DateTime<Utc>>)> {
        let limit = filter.limit.clamp(1, 200);
        let stage_str: Option<&str> = filter.stage.map(|s| s.as_str());
        let rows: Vec<PipelineEvent> = sqlx::query_as(
            "SELECT id, workspace_id, connector_id, stream_id, stage, detail, occurred_at
               FROM pipeline_events
              WHERE workspace_id = $1
                AND ($2::uuid IS NULL OR connector_id = $2)
                AND ($3::text IS NULL OR stage = $3)
                AND ($4::timestamptz IS NULL OR occurred_at < $4)
              ORDER BY occurred_at DESC
              LIMIT $5",
        )
        .bind(workspace_id)
        .bind(filter.connector_id)
        .bind(stage_str)
        .bind(filter.before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list pipeline events")?;
        let next = rows.last().map(|e| e.occurred_at);
        Ok((rows, next))
    }
}
