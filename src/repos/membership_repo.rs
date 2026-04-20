use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Membership;

pub struct MembershipRepo {
    pub pool: PgPool,
}

impl MembershipRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert if absent, return existing row if the unique key already exists.
    pub async fn upsert(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        role_id: Uuid,
    ) -> anyhow::Result<Membership> {
        let inserted: Option<Membership> = sqlx::query_as::<_, Membership>(
            "INSERT INTO memberships (user_id, workspace_id, role_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, workspace_id, role_id) DO NOTHING
             RETURNING id, user_id, workspace_id, role_id, federated_claim_ref, created_at",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(role_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to upsert membership")?;

        if let Some(m) = inserted {
            return Ok(m);
        }

        // Row already existed — fetch it.
        sqlx::query_as::<_, Membership>(
            "SELECT id, user_id, workspace_id, role_id, federated_claim_ref, created_at
             FROM memberships
             WHERE user_id = $1 AND workspace_id = $2 AND role_id = $3
             LIMIT 1",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(role_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to fetch existing membership")
    }

    /// Upsert a membership with an explicit federated_claim_ref. If the row exists
    /// (matched on user_id, workspace_id, role_id), update federated_claim_ref.
    pub async fn upsert_federated(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        role_id: Uuid,
        federated_claim_ref: &str,
    ) -> anyhow::Result<Membership> {
        sqlx::query_as::<_, Membership>(
            "INSERT INTO memberships (user_id, workspace_id, role_id, federated_claim_ref)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id, workspace_id, role_id) DO UPDATE
               SET federated_claim_ref = EXCLUDED.federated_claim_ref
             RETURNING id, user_id, workspace_id, role_id, federated_claim_ref, created_at",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(role_id)
        .bind(federated_claim_ref)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert federated membership")
    }

    pub async fn list_for_user(&self, user_id: Uuid) -> anyhow::Result<Vec<Membership>> {
        sqlx::query_as::<_, Membership>(
            "SELECT id, user_id, workspace_id, role_id, federated_claim_ref, created_at
             FROM memberships
             WHERE user_id = $1
             ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list memberships for user")
    }
}
