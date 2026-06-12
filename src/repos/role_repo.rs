use std::collections::HashSet;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Role;

pub struct RoleRepo {
    pub(crate) pool: PgPool,
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
             ON CONFLICT (workspace_id, name, coc_level) DO UPDATE
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
             ORDER BY coc_level ASC
             LIMIT 1",
        )
        .bind(workspace_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get role by name")
    }

    /// Effective workspace permissions for `(user, workspace)`: the union of
    /// `permissions` arrays across all the user's roles in that workspace,
    /// plus the max `coc_level`. `(empty set, 0)` when the user has no
    /// membership there (fail-closed callers treat that as no access).
    pub async fn effective_permissions(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> anyhow::Result<(HashSet<String>, i32)> {
        let (perms, max_coc): (Option<serde_json::Value>, i32) = sqlx::query_as(
            "SELECT jsonb_agg(r.permissions) AS perms, COALESCE(MAX(r.coc_level), 0) AS max_coc
             FROM memberships m JOIN roles r ON r.id = m.role_id
             WHERE m.user_id = $1 AND m.workspace_id = $2",
        )
        .bind(user_id)
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to load effective permissions")?;

        // Flatten the array-of-arrays; non-array entries (legacy '{}' objects)
        // contribute nothing.
        let mut held = HashSet::new();
        if let Some(serde_json::Value::Array(sets)) = perms {
            for set in sets {
                if let serde_json::Value::Array(items) = set {
                    for item in items {
                        if let serde_json::Value::String(s) = item {
                            held.insert(s);
                        }
                    }
                }
            }
        }
        Ok((held, max_coc))
    }

    pub async fn list(&self, workspace_id: Uuid) -> anyhow::Result<Vec<Role>> {
        sqlx::query_as::<_, Role>(
            "SELECT id, workspace_id, name, coc_level, permissions
             FROM roles
             WHERE workspace_id = $1
             ORDER BY coc_level ASC, name ASC",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list roles for workspace")
    }
}
