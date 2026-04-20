use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Workspace, WorkspaceLifecycle};

pub struct WorkspaceRepo {
    pub pool: PgPool,
}

impl WorkspaceRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        org_id: Uuid,
        name: &str,
        domain: &str,
        lifecycle: WorkspaceLifecycle,
        parent_id: Option<Uuid>,
    ) -> anyhow::Result<Workspace> {
        sqlx::query_as::<_, Workspace>(
            "INSERT INTO workspaces (org_id, name, domain, lifecycle, parent_id)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, org_id, parent_id, name, domain, lifecycle,
                       end_condition, metadata, created_at, closed_at",
        )
        .bind(org_id)
        .bind(name)
        .bind(domain)
        .bind(lifecycle)
        .bind(parent_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to create workspace")
    }

    pub async fn list(&self, org_id: Uuid) -> anyhow::Result<Vec<Workspace>> {
        sqlx::query_as::<_, Workspace>(
            "SELECT id, org_id, parent_id, name, domain, lifecycle,
                    end_condition, metadata, created_at, closed_at
             FROM workspaces
             WHERE org_id = $1
             ORDER BY created_at DESC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workspaces")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Workspace>> {
        sqlx::query_as::<_, Workspace>(
            "SELECT id, org_id, parent_id, name, domain, lifecycle,
                    end_condition, metadata, created_at, closed_at
             FROM workspaces
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get workspace")
    }

    pub async fn close(&self, id: Uuid) -> anyhow::Result<Workspace> {
        sqlx::query_as::<_, Workspace>(
            "UPDATE workspaces
             SET closed_at = now()
             WHERE id = $1
             RETURNING id, org_id, parent_id, name, domain, lifecycle,
                       end_condition, metadata, created_at, closed_at",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .context("failed to close workspace")
    }

    pub async fn find_by_name(
        &self,
        org_id: Uuid,
        name: &str,
    ) -> anyhow::Result<Option<Workspace>> {
        sqlx::query_as::<_, Workspace>(
            "SELECT id, org_id, parent_id, name, domain, lifecycle,
                    end_condition, metadata, created_at, closed_at
             FROM workspaces
             WHERE org_id = $1 AND name = $2
             LIMIT 1",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find workspace by name")
    }
}
