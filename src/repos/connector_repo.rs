use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Connector, ConnectorKind, ConnectorStatus};

pub struct ConnectorRepo {
    pub pool: PgPool,
}

impl ConnectorRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        workspace_id: Uuid,
        kind: ConnectorKind,
        name: &str,
        config: serde_json::Value,
    ) -> anyhow::Result<Connector> {
        sqlx::query_as::<_, Connector>(
            "INSERT INTO connectors (workspace_id, kind, name, config)
             VALUES ($1, $2, $3, $4)
             RETURNING id, workspace_id, kind, name, config, status, last_error, created_at",
        )
        .bind(workspace_id)
        .bind(kind)
        .bind(name)
        .bind(config)
        .fetch_one(&self.pool)
        .await
        .context("failed to create connector")
    }

    pub async fn list(&self, workspace_id: Uuid) -> anyhow::Result<Vec<Connector>> {
        sqlx::query_as::<_, Connector>(
            "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
             FROM connectors
             WHERE workspace_id = $1
             ORDER BY created_at DESC",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list connectors")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Connector>> {
        sqlx::query_as::<_, Connector>(
            "SELECT id, workspace_id, kind, name, config, status, last_error, created_at
             FROM connectors
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get connector")
    }

    pub async fn update_status(
        &self,
        id: Uuid,
        status: ConnectorStatus,
        last_error: Option<&str>,
    ) -> anyhow::Result<Connector> {
        sqlx::query_as::<_, Connector>(
            "UPDATE connectors
             SET status = $2, last_error = $3
             WHERE id = $1
             RETURNING id, workspace_id, kind, name, config, status, last_error, created_at",
        )
        .bind(id)
        .bind(status)
        .bind(last_error)
        .fetch_one(&self.pool)
        .await
        .context("failed to update connector status")
    }
}
