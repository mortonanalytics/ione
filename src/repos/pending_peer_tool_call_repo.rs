use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "pending_peer_tool_call_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PendingPeerToolCallStatus {
    Pending,
    Approved,
    Rejected,
    Executed,
    Expired,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPeerToolCall {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub artifact_id: Uuid,
    pub approval_id: Uuid,
    pub namespaced_tool: String,
    #[serde(skip_serializing)]
    pub arguments_ciphertext: Vec<u8>,
    pub arguments_digest: String,
    pub requested_by: Uuid,
    pub status: PendingPeerToolCallStatus,
    pub expires_at: DateTime<Utc>,
    pub approver_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub executed_at: Option<DateTime<Utc>>,
    pub result_ref: Option<serde_json::Value>,
}

pub struct PendingPeerToolCallRepo {
    pool: PgPool,
}

impl PendingPeerToolCallRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        artifact_id: Uuid,
        approval_id: Uuid,
        namespaced_tool: &str,
        arguments_ciphertext: &[u8],
        arguments_digest: &str,
        requested_by: Uuid,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<PendingPeerToolCall> {
        sqlx::query_as::<_, PendingPeerToolCall>(
            "INSERT INTO pending_peer_tool_calls
               (workspace_id, peer_id, artifact_id, approval_id, namespaced_tool,
                arguments_ciphertext, arguments_digest, requested_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (workspace_id, peer_id, arguments_digest) WHERE status IN ('pending', 'approved')
             DO UPDATE SET namespaced_tool = pending_peer_tool_calls.namespaced_tool
             RETURNING id, workspace_id, peer_id, artifact_id, approval_id, namespaced_tool,
                       arguments_ciphertext, arguments_digest, requested_by, status,
                       expires_at, approver_user_id, created_at, executed_at, result_ref",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(artifact_id)
        .bind(approval_id)
        .bind(namespaced_tool)
        .bind(arguments_ciphertext)
        .bind(arguments_digest)
        .bind(requested_by)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert pending peer tool call")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<PendingPeerToolCall>> {
        sqlx::query_as::<_, PendingPeerToolCall>(
            "SELECT id, workspace_id, peer_id, artifact_id, approval_id, namespaced_tool,
                    arguments_ciphertext, arguments_digest, requested_by, status,
                    expires_at, approver_user_id, created_at, executed_at, result_ref
             FROM pending_peer_tool_calls
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get pending peer tool call")
    }

    pub async fn get_by_approval(
        &self,
        approval_id: Uuid,
    ) -> anyhow::Result<Option<PendingPeerToolCall>> {
        sqlx::query_as::<_, PendingPeerToolCall>(
            "SELECT id, workspace_id, peer_id, artifact_id, approval_id, namespaced_tool,
                    arguments_ciphertext, arguments_digest, requested_by, status,
                    expires_at, approver_user_id, created_at, executed_at, result_ref
             FROM pending_peer_tool_calls
             WHERE approval_id = $1",
        )
        .bind(approval_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get pending peer tool call by approval")
    }

    pub async fn mark_approved(&self, id: Uuid, approver_user_id: Uuid) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE pending_peer_tool_calls
             SET status = 'approved', approver_user_id = $2
             WHERE id = $1 AND status = 'pending' AND expires_at > now()",
        )
        .bind(id)
        .bind(approver_user_id)
        .execute(&self.pool)
        .await
        .context("failed to approve pending peer tool call")?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_rejected(&self, id: Uuid, approver_user_id: Uuid) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE pending_peer_tool_calls
             SET status = 'rejected', approver_user_id = $2
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(id)
        .bind(approver_user_id)
        .execute(&self.pool)
        .await
        .context("failed to reject pending peer tool call")?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_executed(
        &self,
        id: Uuid,
        result_ref: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE pending_peer_tool_calls
             SET status = 'executed', executed_at = now(), result_ref = $2
             WHERE id = $1 AND status = 'approved' AND executed_at IS NULL",
        )
        .bind(id)
        .bind(result_ref)
        .execute(&self.pool)
        .await
        .context("failed to mark pending peer tool call executed")?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn expire_due(&self) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE pending_peer_tool_calls
             SET status = 'expired'
             WHERE status IN ('pending', 'approved') AND expires_at <= now()",
        )
        .execute(&self.pool)
        .await
        .context("failed to expire pending peer tool calls")?;
        Ok(result.rows_affected())
    }
}
