use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::ServiceAccountToken;

const TOKEN_COLUMNS: &str = "id, org_id, name, token_hash, permissions, provisionable_max_coc, \
     created_by, expires_at, revoked_at, last_used_at, created_at, updated_at";

pub struct ServiceAccountTokenRepo {
    pub(crate) pool: PgPool,
}

impl ServiceAccountTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a token row. `token_hash` is the SHA-256 hex of the plaintext,
    /// computed by the caller; the plaintext is never passed here.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue(
        &self,
        org_id: Uuid,
        name: &str,
        token_hash: &str,
        permissions: &serde_json::Value,
        provisionable_max_coc: i32,
        created_by: Option<Uuid>,
        expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<ServiceAccountToken> {
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        let token = sqlx::query_as::<_, ServiceAccountToken>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO service_account_tokens
               (org_id, name, token_hash, permissions, provisionable_max_coc, created_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING {TOKEN_COLUMNS}"
        )))
        .bind(org_id)
        .bind(name)
        .bind(token_hash)
        .bind(permissions)
        .bind(provisionable_max_coc)
        .bind(created_by)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .context("failed to issue service account token")?;
        tx.commit().await?;
        Ok(token)
    }

    /// Active (not-revoked) tokens for an org, newest first.
    pub async fn list_active(&self, org_id: Uuid) -> anyhow::Result<Vec<ServiceAccountToken>> {
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        let tokens = sqlx::query_as::<_, ServiceAccountToken>(sqlx::AssertSqlSafe(format!(
            "SELECT {TOKEN_COLUMNS}
             FROM service_account_tokens
             WHERE org_id = $1 AND revoked_at IS NULL
             ORDER BY created_at DESC"
        )))
        .bind(org_id)
        .fetch_all(&mut *tx)
        .await
        .context("failed to list service account tokens")?;
        tx.commit().await?;
        Ok(tokens)
    }

    /// Soft-delete (revoke) a token in the caller's org. Returns true if a row
    /// was newly revoked, false if not found or already revoked.
    pub async fn revoke(&self, id: Uuid, org_id: Uuid) -> anyhow::Result<bool> {
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query(
            "UPDATE service_account_tokens
             SET revoked_at = now()
             WHERE id = $1 AND org_id = $2 AND revoked_at IS NULL",
        )
        .bind(id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .context("failed to revoke service account token")?
        .rows_affected();
        tx.commit().await?;
        Ok(rows > 0)
    }

    /// Look up a token by hash, accepting only rows that are not revoked and
    /// not expired. Fail-closed: anything else returns `None`.
    ///
    /// The hash is the only thing in hand, so the org comes from the
    /// `resolve_service_account_token_org` resolver (migration 0060), which
    /// returns an org id and nothing else; the row itself is read under that
    /// org's context.
    pub async fn verify(&self, token_hash: &str) -> anyhow::Result<Option<ServiceAccountToken>> {
        let org_id: Option<Uuid> =
            sqlx::query_scalar("SELECT resolve_service_account_token_org($1)")
                .bind(token_hash)
                .fetch_one(&self.pool)
                .await
                .context("failed to resolve service account token org")?;
        let Some(org_id) = org_id else {
            return Ok(None);
        };
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        let token = sqlx::query_as::<_, ServiceAccountToken>(sqlx::AssertSqlSafe(format!(
            "SELECT {TOKEN_COLUMNS}
             FROM service_account_tokens
             WHERE token_hash = $1
               AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > now())"
        )))
        .bind(token_hash)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to verify service account token")?;
        tx.commit().await?;
        Ok(token)
    }

    /// The token's provisioning coc ceiling, read under the caller's org.
    /// `None` when the token is not in that org.
    pub async fn provisionable_max_coc(
        &self,
        org_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<i32>> {
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        let coc = sqlx::query_scalar::<_, i32>(
            "SELECT provisionable_max_coc FROM service_account_tokens WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read service account token coc ceiling")?;
        tx.commit().await?;
        Ok(coc)
    }

    /// Fire-and-forget last-used stamp. Errors are swallowed by the caller.
    pub async fn touch_last_used(&self, org_id: Uuid, id: Uuid) -> anyhow::Result<()> {
        let mut tx = crate::rls::org_scoped_tx(&self.pool, org_id).await?;
        sqlx::query("UPDATE service_account_tokens SET last_used_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to touch service account token last_used_at")?;
        tx.commit().await?;
        Ok(())
    }
}
