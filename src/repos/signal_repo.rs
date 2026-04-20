use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Severity, Signal, SignalSource};

pub struct SignalRepo {
    pub pool: PgPool,
}

impl SignalRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        &self,
        workspace_id: Uuid,
        source: SignalSource,
        title: &str,
        body: &str,
        evidence: serde_json::Value,
        severity: Severity,
        generator_model: Option<&str>,
    ) -> anyhow::Result<Signal> {
        sqlx::query_as::<_, Signal>(
            "INSERT INTO signals (workspace_id, source, title, body, evidence, severity, generator_model)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, workspace_id, source, title, body, evidence, severity, generator_model, created_at",
        )
        .bind(workspace_id)
        .bind(source)
        .bind(title)
        .bind(body)
        .bind(evidence)
        .bind(severity)
        .bind(generator_model)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert signal")
    }

    pub async fn list(
        &self,
        workspace_id: Uuid,
        source_filter: Option<SignalSource>,
        severity_filter: Option<Severity>,
        limit: i64,
    ) -> anyhow::Result<Vec<Signal>> {
        // Build query with optional filters
        match (source_filter, severity_filter) {
            (Some(src), Some(sev)) => {
                sqlx::query_as::<_, Signal>(
                    "SELECT id, workspace_id, source, title, body, evidence, severity, generator_model, created_at
                     FROM signals
                     WHERE workspace_id = $1 AND source = $2 AND severity = $3
                     ORDER BY created_at DESC
                     LIMIT $4",
                )
                .bind(workspace_id)
                .bind(src)
                .bind(sev)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .context("failed to list signals")
            }
            (Some(src), None) => {
                sqlx::query_as::<_, Signal>(
                    "SELECT id, workspace_id, source, title, body, evidence, severity, generator_model, created_at
                     FROM signals
                     WHERE workspace_id = $1 AND source = $2
                     ORDER BY created_at DESC
                     LIMIT $3",
                )
                .bind(workspace_id)
                .bind(src)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .context("failed to list signals")
            }
            (None, Some(sev)) => {
                sqlx::query_as::<_, Signal>(
                    "SELECT id, workspace_id, source, title, body, evidence, severity, generator_model, created_at
                     FROM signals
                     WHERE workspace_id = $1 AND severity = $2
                     ORDER BY created_at DESC
                     LIMIT $3",
                )
                .bind(workspace_id)
                .bind(sev)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .context("failed to list signals")
            }
            (None, None) => {
                sqlx::query_as::<_, Signal>(
                    "SELECT id, workspace_id, source, title, body, evidence, severity, generator_model, created_at
                     FROM signals
                     WHERE workspace_id = $1
                     ORDER BY created_at DESC
                     LIMIT $2",
                )
                .bind(workspace_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .context("failed to list signals")
            }
        }
    }

    /// Returns true if a signal already exists with the same (workspace_id, source, title)
    /// AND its evidence JSONB contains all the given event IDs.
    /// Used for idempotency: prevents inserting duplicate rule-signals for already-seen events.
    pub async fn exists_by_title_for_events(
        &self,
        workspace_id: Uuid,
        source: SignalSource,
        title: &str,
        evidence_event_ids: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM signals
               WHERE workspace_id = $1
                 AND source = $2
                 AND title = $3
                 AND evidence @> $4::jsonb
             )",
        )
        .bind(workspace_id)
        .bind(source)
        .bind(title)
        .bind(evidence_event_ids)
        .fetch_one(&self.pool)
        .await
        .context("failed to check signal existence")?;

        Ok(exists)
    }
}
