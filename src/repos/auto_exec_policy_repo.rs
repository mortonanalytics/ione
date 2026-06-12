use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AutoExecPolicy;

const RETURNING: &str = "id, org_id, workspace_id, name, trigger_signal_title_prefix,
     trigger_severity_at_most, connector_id, op, args_template, rate_limit_per_min,
     severity_cap, authorized_by_permission, enabled, created_by, created_at, updated_at";

/// Validated write payload for create/update (full replace).
pub struct AutoExecPolicyInput {
    pub name: String,
    pub trigger_signal_title_prefix: Option<String>,
    pub trigger_severity_at_most: Option<String>,
    pub connector_id: Uuid,
    pub op: String,
    pub args_template: serde_json::Value,
    pub rate_limit_per_min: i32,
    pub severity_cap: String,
    pub authorized_by_permission: String,
    pub enabled: bool,
}

pub struct AutoExecPolicyRepo {
    pool: PgPool,
}

impl AutoExecPolicyRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> anyhow::Result<Vec<AutoExecPolicy>> {
        sqlx::query_as::<_, AutoExecPolicy>(&format!(
            "SELECT {RETURNING} FROM auto_exec_policies
             WHERE workspace_id = $1
             ORDER BY created_at"
        ))
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list auto_exec policies")
    }

    /// Engine read path: enabled policies only.
    pub async fn list_enabled_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> anyhow::Result<Vec<AutoExecPolicy>> {
        sqlx::query_as::<_, AutoExecPolicy>(&format!(
            "SELECT {RETURNING} FROM auto_exec_policies
             WHERE workspace_id = $1 AND enabled = true
             ORDER BY created_at"
        ))
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list enabled auto_exec policies")
    }

    pub async fn get(
        &self,
        id: Uuid,
        workspace_id: Uuid,
    ) -> anyhow::Result<Option<AutoExecPolicy>> {
        sqlx::query_as::<_, AutoExecPolicy>(&format!(
            "SELECT {RETURNING} FROM auto_exec_policies
             WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(id)
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get auto_exec policy")
    }

    pub async fn create(
        &self,
        workspace_id: Uuid,
        created_by: Uuid,
        input: &AutoExecPolicyInput,
    ) -> anyhow::Result<AutoExecPolicy> {
        sqlx::query_as::<_, AutoExecPolicy>(&format!(
            "INSERT INTO auto_exec_policies
               (org_id, workspace_id, name, trigger_signal_title_prefix,
                trigger_severity_at_most, connector_id, op, args_template,
                rate_limit_per_min, severity_cap, authorized_by_permission,
                enabled, created_by)
             VALUES ((SELECT org_id FROM workspaces WHERE id = $1),
                     $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             RETURNING {RETURNING}"
        ))
        .bind(workspace_id)
        .bind(&input.name)
        .bind(&input.trigger_signal_title_prefix)
        .bind(&input.trigger_severity_at_most)
        .bind(input.connector_id)
        .bind(&input.op)
        .bind(&input.args_template)
        .bind(input.rate_limit_per_min)
        .bind(&input.severity_cap)
        .bind(&input.authorized_by_permission)
        .bind(input.enabled)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await
        .context("failed to create auto_exec policy")
    }

    /// Full replace. Returns None if the policy does not exist in the workspace.
    pub async fn update(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        input: &AutoExecPolicyInput,
    ) -> anyhow::Result<Option<AutoExecPolicy>> {
        sqlx::query_as::<_, AutoExecPolicy>(&format!(
            "UPDATE auto_exec_policies SET
               name = $3,
               trigger_signal_title_prefix = $4,
               trigger_severity_at_most = $5,
               connector_id = $6,
               op = $7,
               args_template = $8,
               rate_limit_per_min = $9,
               severity_cap = $10,
               authorized_by_permission = $11,
               enabled = $12
             WHERE id = $1 AND workspace_id = $2
             RETURNING {RETURNING}"
        ))
        .bind(id)
        .bind(workspace_id)
        .bind(&input.name)
        .bind(&input.trigger_signal_title_prefix)
        .bind(&input.trigger_severity_at_most)
        .bind(input.connector_id)
        .bind(&input.op)
        .bind(&input.args_template)
        .bind(input.rate_limit_per_min)
        .bind(&input.severity_cap)
        .bind(&input.authorized_by_permission)
        .bind(input.enabled)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update auto_exec policy")
    }

    pub async fn delete(&self, id: Uuid, workspace_id: Uuid) -> anyhow::Result<bool> {
        let result =
            sqlx::query("DELETE FROM auto_exec_policies WHERE id = $1 AND workspace_id = $2")
                .bind(id)
                .bind(workspace_id)
                .execute(&self.pool)
                .await
                .context("failed to delete auto_exec policy")?;
        Ok(result.rows_affected() > 0)
    }
}
