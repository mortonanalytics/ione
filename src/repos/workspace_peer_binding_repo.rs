use anyhow::Context;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    models::{BindingStatus, WorkspacePeerBinding},
    services::workspace_peer_binding::WhoamiResponse,
};

pub struct WorkspacePeerBindingRepo {
    pool: PgPool,
}

impl WorkspacePeerBindingRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_from_subscribe(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        whoami: Option<&WhoamiResponse>,
    ) -> anyhow::Result<WorkspacePeerBinding> {
        let (
            foreign_tenant_id,
            foreign_tenant_name,
            foreign_workspace_id,
            foreign_user_id,
            foreign_user_email,
            foreign_roles,
            status,
        ) = match whoami {
            Some(w) => (
                w.foreign_tenant_id.as_str(),
                w.foreign_tenant_name.as_deref(),
                w.foreign_workspace_id.as_deref(),
                w.foreign_user_id.as_deref(),
                w.foreign_user_email.as_deref(),
                w.foreign_roles.clone(),
                BindingStatus::Active,
            ),
            None => (
                "",
                None,
                None,
                None,
                None,
                Vec::new(),
                BindingStatus::Pending,
            ),
        };

        sqlx::query_as::<_, WorkspacePeerBinding>(
            "INSERT INTO workspace_peer_bindings
               (workspace_id, peer_id, foreign_tenant_id, foreign_tenant_name,
                foreign_workspace_id, foreign_user_id, foreign_user_email,
                foreign_roles, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (workspace_id, peer_id) DO UPDATE SET
                foreign_tenant_name = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN EXCLUDED.foreign_tenant_name
                    ELSE workspace_peer_bindings.foreign_tenant_name
                END,
                foreign_workspace_id = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN EXCLUDED.foreign_workspace_id
                    ELSE workspace_peer_bindings.foreign_workspace_id
                END,
                foreign_user_id = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN EXCLUDED.foreign_user_id
                    ELSE workspace_peer_bindings.foreign_user_id
                END,
                foreign_user_email = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN EXCLUDED.foreign_user_email
                    ELSE workspace_peer_bindings.foreign_user_email
                END,
                foreign_roles = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN EXCLUDED.foreign_roles
                    ELSE workspace_peer_bindings.foreign_roles
                END,
                foreign_tenant_id = CASE
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' THEN EXCLUDED.foreign_tenant_id
                    ELSE workspace_peer_bindings.foreign_tenant_id
                END,
                status = CASE
                    WHEN EXCLUDED.foreign_tenant_id = '' THEN workspace_peer_bindings.status
                    WHEN workspace_peer_bindings.foreign_tenant_id = '' OR workspace_peer_bindings.foreign_tenant_id = EXCLUDED.foreign_tenant_id
                    THEN 'active'::binding_status
                    ELSE 'conflict'::binding_status
                END,
                whoami_refreshed_at = now()
             RETURNING id, org_id, workspace_id, peer_id, foreign_tenant_id,
                       foreign_tenant_name, foreign_workspace_id, foreign_user_id,
                       foreign_user_email, foreign_roles, scope, status,
                       whoami_refreshed_at, created_at, updated_at",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(foreign_tenant_id)
        .bind(foreign_tenant_name)
        .bind(foreign_workspace_id)
        .bind(foreign_user_id)
        .bind(foreign_user_email)
        .bind(foreign_roles)
        .bind(status)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert workspace peer binding")
    }

    pub async fn get_by_workspace_peer(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Option<WorkspacePeerBinding>> {
        sqlx::query_as::<_, WorkspacePeerBinding>(&select_binding_sql(
            "WHERE workspace_id = $1 AND peer_id = $2",
        ))
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get workspace peer binding")
    }

    pub async fn list_by_workspace(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<WorkspacePeerBinding>> {
        sqlx::query_as::<_, WorkspacePeerBinding>(
            &select_binding_sql(
                "WHERE b.workspace_id = $1
                   AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2)",
            ),
        )
        .bind(workspace_id)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workspace peer bindings")
    }

    pub async fn list_by_peer(
        &self,
        peer_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<WorkspacePeerBinding>> {
        sqlx::query_as::<_, WorkspacePeerBinding>(
            &select_binding_sql(
                "WHERE b.peer_id = $1
                   AND EXISTS (SELECT 1 FROM peers p WHERE p.id = b.peer_id AND p.org_id = $2)
                   AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2)",
            ),
        )
        .bind(peer_id)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list peer bindings")
    }

    /// Returns all active Peer records that have an active binding to `workspace_id`
    /// within `org_id`. Used by the map-layer fan-out service.
    pub async fn list_active_peers_for_workspace(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<crate::models::Peer>> {
        sqlx::query_as::<_, crate::models::Peer>(
            "SELECT p.id, p.org_id, p.name, p.mcp_url, p.issuer_id, p.sharing_policy,
                    p.status, p.created_at, p.oauth_client_id,
                    p.access_token_hash, p.refresh_token_hash,
                    p.access_token_ciphertext, p.refresh_token_ciphertext,
                    p.token_expires_at, p.tool_allowlist
             FROM workspace_peer_bindings b
             JOIN peers p ON p.id = b.peer_id
             WHERE b.workspace_id = $1
               AND b.status = 'active'
               AND p.status = 'active'
               AND p.org_id = $2
               AND EXISTS (
                   SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2
               )
             ORDER BY b.created_at DESC",
        )
        .bind(workspace_id)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list active peers for workspace")
    }

    pub async fn get_by_id_org_scoped(
        &self,
        id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Option<WorkspacePeerBinding>> {
        sqlx::query_as::<_, WorkspacePeerBinding>(
            &select_binding_sql(
                "WHERE b.id = $1
                   AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2)",
            ),
        )
        .bind(id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get org-scoped binding")
    }

    pub async fn create_manual(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        org_id: Uuid,
        foreign_tenant_id: &str,
        foreign_workspace_id: Option<&str>,
        scope: Value,
    ) -> anyhow::Result<WorkspacePeerBinding> {
        let allowed: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM workspaces w, peers p
                WHERE w.id = $1 AND p.id = $2
                  AND w.org_id = $3 AND p.org_id = $3
             )",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to validate binding ownership")?;
        if !allowed {
            anyhow::bail!("binding target not found");
        }
        let duplicate_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM workspace_peer_bindings
                WHERE workspace_id = $1 AND peer_id = $2
             )",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to check duplicate workspace peer binding")?;
        if duplicate_exists {
            anyhow::bail!("duplicate workspace peer binding");
        }

        sqlx::query_as::<_, WorkspacePeerBinding>(
            "INSERT INTO workspace_peer_bindings
               (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, scope, status)
             VALUES ($1, $2, $3, $4, $5, 'active'::binding_status)
             RETURNING id, org_id, workspace_id, peer_id, foreign_tenant_id,
                       foreign_tenant_name, foreign_workspace_id, foreign_user_id,
                       foreign_user_email, foreign_roles, scope, status,
                       whoami_refreshed_at, created_at, updated_at",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(foreign_tenant_id)
        .bind(foreign_workspace_id)
        .bind(scope)
        .fetch_one(&self.pool)
        .await
        .context("failed to create manual workspace peer binding")
    }

    pub async fn patch(
        &self,
        id: Uuid,
        org_id: Uuid,
        foreign_tenant_id: Option<&str>,
        foreign_workspace_id: Option<Option<&str>>,
        scope: Option<Value>,
    ) -> anyhow::Result<Option<WorkspacePeerBinding>> {
        let Some(current) = self.get_by_id_org_scoped(id, org_id).await? else {
            return Ok(None);
        };

        let next_tenant = foreign_tenant_id.unwrap_or(&current.foreign_tenant_id);
        let next_workspace =
            foreign_workspace_id.unwrap_or(current.foreign_workspace_id.as_deref());
        let next_scope = scope.unwrap_or(current.scope);
        let next_status = if current.status == BindingStatus::Pending && !next_tenant.is_empty() {
            BindingStatus::Active
        } else {
            current.status
        };

        sqlx::query_as::<_, WorkspacePeerBinding>(
            "UPDATE workspace_peer_bindings
             SET foreign_tenant_id = $2,
                 foreign_workspace_id = $3,
                 scope = $4,
                 status = $5
             WHERE id = $1
             RETURNING id, org_id, workspace_id, peer_id, foreign_tenant_id,
                       foreign_tenant_name, foreign_workspace_id, foreign_user_id,
                       foreign_user_email, foreign_roles, scope, status,
                       whoami_refreshed_at, created_at, updated_at",
        )
        .bind(id)
        .bind(next_tenant)
        .bind(next_workspace)
        .bind(next_scope)
        .bind(next_status)
        .fetch_optional(&self.pool)
        .await
        .context("failed to patch workspace peer binding")
    }

    pub async fn delete_by_id_org_scoped(&self, id: Uuid, org_id: Uuid) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workspace_peer_bindings b
             WHERE b.id = $1
               AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = b.workspace_id AND w.org_id = $2)",
        )
        .bind(id)
        .bind(org_id)
        .execute(&self.pool)
        .await
        .context("failed to delete workspace peer binding")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn apply_whoami_refresh(
        &self,
        id: Uuid,
        org_id: Uuid,
        whoami: &WhoamiResponse,
    ) -> anyhow::Result<Option<(WorkspacePeerBinding, bool)>> {
        let Some(current) = self.get_by_id_org_scoped(id, org_id).await? else {
            return Ok(None);
        };
        let drift = !current.foreign_tenant_id.is_empty()
            && current.foreign_tenant_id != whoami.foreign_tenant_id;
        if drift {
            let row = sqlx::query_as::<_, WorkspacePeerBinding>(
                "UPDATE workspace_peer_bindings
                 SET status = 'conflict'::binding_status,
                     whoami_refreshed_at = now()
                 WHERE id = $1
                 RETURNING id, org_id, workspace_id, peer_id, foreign_tenant_id,
                           foreign_tenant_name, foreign_workspace_id, foreign_user_id,
                           foreign_user_email, foreign_roles, scope, status,
                           whoami_refreshed_at, created_at, updated_at",
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("failed to mark binding conflict")?;
            return Ok(Some((row, true)));
        }

        let row = sqlx::query_as::<_, WorkspacePeerBinding>(
            "UPDATE workspace_peer_bindings
             SET foreign_tenant_id = $2,
                 foreign_tenant_name = $3,
                 foreign_workspace_id = $4,
                 foreign_user_id = $5,
                 foreign_user_email = $6,
                 foreign_roles = $7,
                 status = 'active'::binding_status,
                 whoami_refreshed_at = now()
             WHERE id = $1
             RETURNING id, org_id, workspace_id, peer_id, foreign_tenant_id,
                       foreign_tenant_name, foreign_workspace_id, foreign_user_id,
                       foreign_user_email, foreign_roles, scope, status,
                       whoami_refreshed_at, created_at, updated_at",
        )
        .bind(id)
        .bind(&whoami.foreign_tenant_id)
        .bind(&whoami.foreign_tenant_name)
        .bind(&whoami.foreign_workspace_id)
        .bind(&whoami.foreign_user_id)
        .bind(&whoami.foreign_user_email)
        .bind(&whoami.foreign_roles)
        .fetch_one(&self.pool)
        .await
        .context("failed to refresh workspace peer binding")?;
        Ok(Some((row, false)))
    }

    pub async fn set_inactive_for_peer(&self, peer_id: Uuid) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE workspace_peer_bindings
             SET status = 'inactive'::binding_status
             WHERE peer_id = $1
               AND status <> 'inactive'::binding_status",
        )
        .bind(peer_id)
        .execute(&self.pool)
        .await
        .context("failed to inactivate peer bindings")?;
        Ok(result.rows_affected())
    }

    pub async fn peer_for_binding(&self, id: Uuid, org_id: Uuid) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query(
            "SELECT b.peer_id
             FROM workspace_peer_bindings b
             JOIN workspaces w ON w.id = b.workspace_id
             WHERE b.id = $1 AND w.org_id = $2",
        )
        .bind(id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get peer for binding")?;
        Ok(row.map(|r| r.get("peer_id")))
    }
}

fn select_binding_sql(where_clause: &str) -> String {
    format!(
        "SELECT b.id, b.org_id, b.workspace_id, b.peer_id, b.foreign_tenant_id,
                b.foreign_tenant_name, b.foreign_workspace_id, b.foreign_user_id,
                b.foreign_user_email, b.foreign_roles, b.scope, b.status,
                b.whoami_refreshed_at, b.created_at, b.updated_at
         FROM workspace_peer_bindings b
         {where_clause}
         ORDER BY b.created_at DESC"
    )
}
