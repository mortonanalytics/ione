use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::UserSession;

pub struct UserSessionRepo {
    pool: PgPool,
}

impl UserSessionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        idp_type: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<UserSession> {
        sqlx::query_as::<_, UserSession>(
            "INSERT INTO user_sessions (user_id, org_id, idp_type, expires_at)
             VALUES ($1, $2, $3, $4)
             RETURNING id, user_id, org_id, idp_type, mfa_verified, expires_at, revoked_at, created_at",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(idp_type)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to create user session")
    }

    pub async fn find_active(&self, id: Uuid) -> anyhow::Result<Option<UserSession>> {
        sqlx::query_as::<_, UserSession>(
            "SELECT id, user_id, org_id, idp_type, mfa_verified, expires_at, revoked_at, created_at
             FROM user_sessions
             WHERE id = $1 AND revoked_at IS NULL AND expires_at > now()",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find active session")
    }

    pub async fn revoke(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE user_sessions SET revoked_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to revoke session")?;
        Ok(())
    }

    pub async fn mark_mfa_verified(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE user_sessions SET mfa_verified = true WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to mark session mfa verified")?;
        Ok(())
    }
}
