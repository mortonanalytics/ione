use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Approval, ApprovalStatus};

pub struct ApprovalRepo {
    pub pool: PgPool,
}

impl ApprovalRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_pending(&self, artifact_id: Uuid) -> anyhow::Result<Approval> {
        sqlx::query_as::<_, Approval>(
            "INSERT INTO approvals (artifact_id, status)
             VALUES ($1, 'pending'::approval_status)
             RETURNING id, artifact_id, approver_user_id, status, comment, decided_at",
        )
        .bind(artifact_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to create pending approval")
    }

    /// List approvals for a workspace, with optional status filter.
    /// JOINs on artifacts to scope by workspace_id.
    pub async fn list(
        &self,
        workspace_id: Uuid,
        status_filter: Option<ApprovalStatus>,
    ) -> anyhow::Result<Vec<Approval>> {
        match status_filter {
            Some(status) => sqlx::query_as::<_, Approval>(
                "SELECT ap.id, ap.artifact_id, ap.approver_user_id, ap.status,
                            ap.comment, ap.decided_at
                     FROM approvals ap
                     JOIN artifacts art ON art.id = ap.artifact_id
                     WHERE art.workspace_id = $1
                       AND ap.status = $2
                     ORDER BY ap.decided_at DESC NULLS FIRST",
            )
            .bind(workspace_id)
            .bind(status)
            .fetch_all(&self.pool)
            .await
            .context("failed to list approvals with status filter"),
            None => sqlx::query_as::<_, Approval>(
                "SELECT ap.id, ap.artifact_id, ap.approver_user_id, ap.status,
                            ap.comment, ap.decided_at
                     FROM approvals ap
                     JOIN artifacts art ON art.id = ap.artifact_id
                     WHERE art.workspace_id = $1
                     ORDER BY ap.decided_at DESC NULLS FIRST",
            )
            .bind(workspace_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list all approvals for workspace"),
        }
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Approval>> {
        sqlx::query_as::<_, Approval>(
            "SELECT id, artifact_id, approver_user_id, status, comment, decided_at
             FROM approvals
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get approval")
    }

    /// Update an approval to a terminal state only if it is currently `pending`.
    ///
    /// Idempotent: if the row is already in the requested terminal state, returns
    /// the existing row unchanged so callers can detect races vs. true no-ops.
    pub async fn decide(
        &self,
        id: Uuid,
        approver_user_id: Uuid,
        decision: ApprovalStatus,
        comment: Option<&str>,
    ) -> anyhow::Result<Approval> {
        // Attempt to update only if currently pending.
        let updated = sqlx::query_as::<_, Approval>(
            "UPDATE approvals
             SET status = $2,
                 approver_user_id = $3,
                 comment = $4,
                 decided_at = now()
             WHERE id = $1
               AND status = 'pending'::approval_status
             RETURNING id, artifact_id, approver_user_id, status, comment, decided_at",
        )
        .bind(id)
        .bind(decision)
        .bind(approver_user_id)
        .bind(comment)
        .fetch_optional(&self.pool)
        .await
        .context("failed to decide approval")?;

        match updated {
            Some(row) => Ok(row),
            None => {
                // Row was not pending — return the current state.
                sqlx::query_as::<_, Approval>(
                    "SELECT id, artifact_id, approver_user_id, status, comment, decided_at
                     FROM approvals
                     WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .context("failed to fetch approval after decide no-op")
            }
        }
    }
}
