use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Stream;

pub struct StreamRepo {
    pub pool: PgPool,
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
    ) -> anyhow::Result<Stream> {
        sqlx::query_as::<_, Stream>(
            "INSERT INTO streams (connector_id, name, schema)
             VALUES ($1, $2, $3)
             ON CONFLICT (connector_id, name)
             DO UPDATE SET schema = EXCLUDED.schema
             RETURNING id, connector_id, name, schema, created_at",
        )
        .bind(connector_id)
        .bind(name)
        .bind(schema)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert stream")
    }

    pub async fn list(&self, connector_id: Uuid) -> anyhow::Result<Vec<Stream>> {
        sqlx::query_as::<_, Stream>(
            "SELECT id, connector_id, name, schema, created_at
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
            "SELECT id, connector_id, name, schema, created_at
             FROM streams
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get stream")
    }
}
