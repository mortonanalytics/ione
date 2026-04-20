use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Artifact, ArtifactKind};

pub struct ArtifactRepo {
    pub pool: PgPool,
}

impl ArtifactRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        workspace_id: Uuid,
        kind: ArtifactKind,
        source_survivor_id: Option<Uuid>,
        content: serde_json::Value,
        blob_ref: Option<&str>,
    ) -> anyhow::Result<Artifact> {
        sqlx::query_as::<_, Artifact>(
            "INSERT INTO artifacts (workspace_id, kind, source_survivor_id, content, blob_ref)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, workspace_id, kind, source_survivor_id, content, blob_ref, created_at",
        )
        .bind(workspace_id)
        .bind(kind)
        .bind(source_survivor_id)
        .bind(content)
        .bind(blob_ref)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert artifact")
    }

    pub async fn list(&self, workspace_id: Uuid, limit: i64) -> anyhow::Result<Vec<Artifact>> {
        sqlx::query_as::<_, Artifact>(
            "SELECT id, workspace_id, kind, source_survivor_id, content, blob_ref, created_at
             FROM artifacts
             WHERE workspace_id = $1
             ORDER BY created_at DESC
             LIMIT $2",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list artifacts")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Artifact>> {
        sqlx::query_as::<_, Artifact>(
            "SELECT id, workspace_id, kind, source_survivor_id, content, blob_ref, created_at
             FROM artifacts
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get artifact")
    }
}
