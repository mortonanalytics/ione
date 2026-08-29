//! Workspace relay mappings and run links.
//!
//! Every method runs inside an org-scoped transaction, so the row-level
//! security policy declared in migration 0052 actually applies. A method that
//! took the pool directly would work today and stop being isolated the moment
//! the runtime moves to the restricted role, which is the point of the policy.

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    models::{NewRelayMapping, RelayRunLink, WorkspaceRelayMapping},
    rls::org_scoped_tx,
};

pub struct RelayMappingRepo {
    pool: PgPool,
}

const MAPPING_COLUMNS: &str = "id, org_id, workspace_id, relay_connection_id, display_name, \
     alias, principal_id, enabled, created_by, created_at, updated_at";

impl RelayMappingRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Mappings this user may actually use in this workspace: enabled, and
    /// either workspace-wide or named to them.
    ///
    /// This is IONe's half of the answer. The caller still asks relay which
    /// connections it will honour, and shows the intersection -- a mapping
    /// pointing at a connection relay has since revoked is a name for nothing.
    pub async fn effective_for_user(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<WorkspaceRelayMapping>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query_as::<_, WorkspaceRelayMapping>(&format!(
            "SELECT {MAPPING_COLUMNS} FROM workspace_relay_mappings \
             WHERE workspace_id = $1 AND enabled \
               AND (principal_id IS NULL OR principal_id = $2) \
             ORDER BY display_name"
        ))
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    /// Every mapping in the workspace, for the admin surface.
    pub async fn list_for_workspace(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
    ) -> anyhow::Result<Vec<WorkspaceRelayMapping>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query_as::<_, WorkspaceRelayMapping>(&format!(
            "SELECT {MAPPING_COLUMNS} FROM workspace_relay_mappings \
             WHERE workspace_id = $1 ORDER BY display_name"
        ))
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    pub async fn by_alias(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        alias: &str,
    ) -> anyhow::Result<Option<WorkspaceRelayMapping>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row = sqlx::query_as::<_, WorkspaceRelayMapping>(&format!(
            "SELECT {MAPPING_COLUMNS} FROM workspace_relay_mappings \
             WHERE workspace_id = $1 AND alias = $2"
        ))
        .bind(workspace_id)
        .bind(alias)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn create(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        created_by: Uuid,
        input: NewRelayMapping,
    ) -> anyhow::Result<WorkspaceRelayMapping> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row = sqlx::query_as::<_, WorkspaceRelayMapping>(&format!(
            "INSERT INTO workspace_relay_mappings \
               (org_id, workspace_id, relay_connection_id, display_name, alias, principal_id, \
                created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {MAPPING_COLUMNS}"
        ))
        .bind(org_id)
        .bind(workspace_id)
        .bind(input.relay_connection_id)
        .bind(input.display_name.trim())
        .bind(input.alias.trim())
        .bind(input.principal_id)
        .bind(created_by)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Disable rather than delete. A mapping that was used is part of how a
    /// past run is explained, and deleting it leaves a run link pointing at
    /// nothing.
    pub async fn set_enabled(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        id: Uuid,
        enabled: bool,
    ) -> anyhow::Result<Option<WorkspaceRelayMapping>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row = sqlx::query_as::<_, WorkspaceRelayMapping>(&format!(
            "UPDATE workspace_relay_mappings SET enabled = $3, updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 RETURNING {MAPPING_COLUMNS}"
        ))
        .bind(id)
        .bind(workspace_id)
        .bind(enabled)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Record what a run did. Counts and hashes; no values.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_run(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        user_id: Uuid,
        relay_run_id: Uuid,
        outcome: &str,
        mapping_ids: &[Uuid],
        row_count: Option<i64>,
        truncated: bool,
        refusal_reason: Option<&str>,
        plan_hash: Option<&str>,
        policy_hash: Option<&str>,
        usage: serde_json::Value,
    ) -> anyhow::Result<Uuid> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO relay_run_links \
               (org_id, workspace_id, user_id, relay_run_id, outcome, mapping_ids, row_count, \
                truncated, refusal_reason, plan_hash, policy_hash, usage) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             ON CONFLICT (org_id, relay_run_id) DO UPDATE \
               SET outcome = EXCLUDED.outcome, \
                   row_count = EXCLUDED.row_count, \
                   truncated = EXCLUDED.truncated, \
                   refusal_reason = EXCLUDED.refusal_reason, \
                   plan_hash = EXCLUDED.plan_hash, \
                   policy_hash = EXCLUDED.policy_hash, \
                   usage = EXCLUDED.usage \
             RETURNING id",
        )
        .bind(org_id)
        .bind(workspace_id)
        .bind(user_id)
        .bind(relay_run_id)
        .bind(outcome)
        .bind(mapping_ids)
        .bind(row_count)
        .bind(truncated)
        .bind(refusal_reason)
        .bind(plan_hash)
        .bind(policy_hash)
        .bind(&usage)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn recent_runs(
        &self,
        org_id: Uuid,
        workspace_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<RelayRunLink>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query_as::<_, RelayRunLink>(
            "SELECT id, org_id, workspace_id, user_id, relay_run_id, outcome, mapping_ids, \
                    row_count, truncated, refusal_reason, plan_hash, policy_hash, usage, created_at \
             FROM relay_run_links WHERE workspace_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }
}
