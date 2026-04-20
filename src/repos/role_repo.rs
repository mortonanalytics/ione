use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Role;

pub struct RoleRepo {
    pub pool: PgPool,
}

impl RoleRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert(
        &self,
        workspace_id: Uuid,
        name: &str,
        coc_level: i32,
    ) -> anyhow::Result<Role> {
        sqlx::query_as::<_, Role>(
            "INSERT INTO roles (workspace_id, name, coc_level)
             VALUES ($1, $2, $3)
             ON CONFLICT (workspace_id, name) DO UPDATE
               SET coc_level = EXCLUDED.coc_level
             RETURNING id, workspace_id, name, coc_level, permissions",
        )
        .bind(workspace_id)
        .bind(name)
        .bind(coc_level)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert role")
    }

    pub async fn get_by_name(
        &self,
        workspace_id: Uuid,
        name: &str,
    ) -> anyhow::Result<Option<Role>> {
        sqlx::query_as::<_, Role>(
            "SELECT id, workspace_id, name, coc_level, permissions
             FROM roles
             WHERE workspace_id = $1 AND name = $2
             LIMIT 1",
        )
        .bind(workspace_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get role by name")
    }
}
