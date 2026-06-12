use std::collections::HashSet;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Role;

/// The workspace grant set given to admin (coc >= 80) roles, matching the
/// migration-0039 backfill. peers:manage is org-scoped and lives in
/// `ORG_ADMIN_GRANTS`; workspace admins pass workspace-scoped checks via the
/// `admin` short-circuit.
pub const WORKSPACE_ADMIN_GRANTS: &str =
    r#"["admin","audit:read","roles:manage","approvals:decide","workspace:write","tool_invoke:*:*"]"#;

/// The org grant set given alongside an admin role, matching the
/// migration-0039 org backfill.
pub const ORG_ADMIN_GRANTS: &[&str] = &["trust_issuers:manage", "peers:manage"];

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
        // Admin roles created after the 0039 backfill get the same workspace
        // grant set inline; never-set ('{}') permissions on an existing row
        // are upgraded, manually-edited arrays are left alone.
        sqlx::query_as::<_, Role>(
            "INSERT INTO roles (workspace_id, name, coc_level, permissions)
             VALUES ($1, $2, $3,
                     CASE WHEN $3 >= 80 THEN $4::jsonb ELSE '{}'::jsonb END)
             ON CONFLICT (workspace_id, name, coc_level) DO UPDATE
               SET coc_level = EXCLUDED.coc_level,
                   permissions = CASE WHEN roles.permissions = '{}'::jsonb
                                      THEN EXCLUDED.permissions
                                      ELSE roles.permissions END
             RETURNING id, workspace_id, name, coc_level, permissions",
        )
        .bind(workspace_id)
        .bind(name)
        .bind(coc_level)
        .bind(WORKSPACE_ADMIN_GRANTS)
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
