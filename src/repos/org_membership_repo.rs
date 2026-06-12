use std::collections::HashSet;

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

pub struct OrgMembershipRepo {
    pub(crate) pool: PgPool,
}

impl OrgMembershipRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Org-scoped permission strings for `(user, org)`. Empty set when no row
    /// exists (fail-closed callers treat that as no access).
    pub async fn org_permissions(
        &self,
        user_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<HashSet<String>> {
        let perms: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT permissions FROM org_memberships WHERE user_id = $1 AND org_id = $2",
        )
        .bind(user_id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load org permissions")?;

        let mut held = HashSet::new();
        if let Some(serde_json::Value::Array(items)) = perms {
            for item in items {
                if let serde_json::Value::String(s) = item {
                    held.insert(s);
                }
            }
        }
        Ok(held)
    }

    /// Grant org-scoped permissions, unioning with any existing grant set.
    pub async fn grant(&self, user_id: Uuid, org_id: Uuid, perms: &[&str]) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO org_memberships (user_id, org_id, permissions)
             VALUES ($1, $2, $3::jsonb)
             ON CONFLICT (user_id, org_id) DO UPDATE
               SET permissions = (
                 SELECT COALESCE(jsonb_agg(DISTINCT p), '[]'::jsonb)
                 FROM jsonb_array_elements(org_memberships.permissions || EXCLUDED.permissions) AS p
               )",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(serde_json::json!(perms))
        .execute(&self.pool)
        .await
        .context("failed to grant org permissions")?;
        Ok(())
    }
}
