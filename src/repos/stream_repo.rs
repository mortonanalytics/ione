use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Stream;

pub struct StreamRepo {
    pub(crate) pool: PgPool,
}

impl StreamRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_named(
        &self,
        connector_id: Uuid,
        name: &str,
        schema: serde_json::Value,
        view_config: Option<serde_json::Value>,
    ) -> anyhow::Result<Stream> {
        sqlx::query_as::<_, Stream>(
            "INSERT INTO streams (connector_id, name, schema, view_config)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (connector_id, name)
             DO UPDATE SET schema = EXCLUDED.schema, view_config = EXCLUDED.view_config
             RETURNING id, connector_id, name, schema, view_config, created_at",
        )
        .bind(connector_id)
        .bind(name)
        .bind(schema)
        .bind(view_config)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert stream")
    }

    pub async fn list(&self, connector_id: Uuid) -> anyhow::Result<Vec<Stream>> {
        sqlx::query_as::<_, Stream>(
            "SELECT id, connector_id, name, schema, view_config, created_at
             FROM streams
             WHERE connector_id = $1
             ORDER BY created_at ASC",
        )
        .bind(connector_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list streams")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Stream>> {
        sqlx::query_as::<_, Stream>(
            "SELECT id, connector_id, name, schema, view_config, created_at
             FROM streams
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get stream")
    }

    pub async fn get_in_org(&self, id: Uuid, org_id: Uuid) -> anyhow::Result<Option<Stream>> {
        sqlx::query_as::<_, Stream>(
            "SELECT s.id, s.connector_id, s.name, s.schema, s.view_config, s.created_at
             FROM streams s
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE s.id = $1 AND w.org_id = $2",
        )
        .bind(id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get stream in org")
    }

    pub async fn list_in_org(
        &self,
        connector_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<Stream>> {
        sqlx::query_as::<_, Stream>(
            "SELECT s.id, s.connector_id, s.name, s.schema, s.view_config, s.created_at
             FROM streams s
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.id = $1 AND w.org_id = $2
             ORDER BY s.created_at ASC",
        )
        .bind(connector_id)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list streams in org")
    }

    pub async fn update_view_config(
        &self,
        id: Uuid,
        view_config: Option<serde_json::Value>,
    ) -> anyhow::Result<Stream> {
        sqlx::query_as::<_, Stream>(
            "UPDATE streams
             SET view_config = $2
             WHERE id = $1
             RETURNING id, connector_id, name, schema, view_config, created_at",
        )
        .bind(id)
        .bind(view_config)
        .fetch_one(&self.pool)
        .await
        .context("failed to update stream view_config")
    }
}
