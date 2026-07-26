use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{models::BrokerCredential, rls::org_scoped_tx};

pub struct BrokerCredentialRepo {
    pool: PgPool,
}

impl BrokerCredentialRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Org-scoped: runs inside an `app.current_org_id` transaction, so the
    /// `broker_credentials_org_isolation` policy is a second guard behind the
    /// `org_id` predicate when the process connects as a non-BYPASSRLS role.
    pub async fn create_pending(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        provider: &str,
        label: &str,
        scopes: &[String],
        state_token: &str,
        code_verifier: &str,
        state_expires_at: DateTime<Utc>,
    ) -> anyhow::Result<BrokerCredential> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row = sqlx::query_as::<_, BrokerCredential>(
            "INSERT INTO broker_credentials
                (user_id, org_id, provider, label, scopes, state_token, code_verifier, state_expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(provider)
        .bind(label)
        .bind(scopes)
        .bind(state_token)
        .bind(code_verifier)
        .bind(state_expires_at)
        .fetch_one(&mut *tx)
        .await
        .context("failed to create pending broker credential")?;
        tx.commit()
            .await
            .context("failed to commit pending broker credential")?;
        Ok(row)
    }

    /// Org-scoped; see [`BrokerCredentialRepo::create_pending`]. This is the read
    /// AC-15 names: a caller in org A cannot see org B's connections even if the
    /// `user_id` predicate were wrong.
    pub async fn list_for_user(
        &self,
        user_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<BrokerCredential>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query_as::<_, BrokerCredential>(
            "SELECT id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at
             FROM broker_credentials
             WHERE user_id = $1 AND org_id = $2
             ORDER BY created_at DESC",
        )
        .bind(user_id)
        .bind(org_id)
        .fetch_all(&mut *tx)
        .await
        .context("failed to list broker credentials")?;
        tx.commit()
            .await
            .context("failed to commit broker credential list")?;
        Ok(rows)
    }

    /// Not org-scoped, and cannot be: the OAuth callback arrives with only the
    /// `state` token, which is what establishes which org the flow belongs to.
    /// Same for [`BrokerCredentialRepo::consume_by_state`]. Listed as uncovered
    /// under AC-15 in md/design/identity-broker.md.
    pub async fn find_by_state(
        &self,
        state_token: &str,
    ) -> anyhow::Result<Option<BrokerCredential>> {
        sqlx::query_as::<_, BrokerCredential>(
            "SELECT id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at
             FROM broker_credentials
             WHERE state_token = $1",
        )
        .bind(state_token)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find broker credential by state")
    }

    pub async fn consume_by_state(
        &self,
        state_token: &str,
    ) -> anyhow::Result<Option<BrokerCredential>> {
        sqlx::query_as::<_, BrokerCredential>(
            "UPDATE broker_credentials
             SET state_token = NULL
             WHERE state_token = $1 AND state_expires_at > now()
             RETURNING id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at",
        )
        .bind(state_token)
        .fetch_optional(&self.pool)
        .await
        .context("failed to consume broker credential state")
    }

    /// One connection by id, scoped to its owner so a caller cannot address
    /// another operator's connection by guessing the uuid. Org-scoped; see
    /// [`BrokerCredentialRepo::create_pending`].
    pub async fn find_for_user(
        &self,
        user_id: Uuid,
        id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Option<BrokerCredential>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row = sqlx::query_as::<_, BrokerCredential>(
            "SELECT id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at
             FROM broker_credentials
             WHERE user_id = $1 AND id = $2 AND org_id = $3",
        )
        .bind(user_id)
        .bind(id)
        .bind(org_id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to find broker credential by id")?;
        tx.commit()
            .await
            .context("failed to commit broker credential read")?;
        Ok(row)
    }

    /// Not org-scoped: no caller has an org id in hand here. Listed as uncovered
    /// under AC-15 in md/design/identity-broker.md.
    pub async fn find_user_provider(
        &self,
        user_id: Uuid,
        provider: &str,
    ) -> anyhow::Result<Option<BrokerCredential>> {
        sqlx::query_as::<_, BrokerCredential>(
            "SELECT id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at
             FROM broker_credentials
             WHERE user_id = $1 AND provider = $2 AND access_token_ciphertext IS NOT NULL
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(user_id)
        .bind(provider)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find broker credential")
    }

    /// Org-scoped; see [`BrokerCredentialRepo::create_pending`]. Both callers
    /// pass the org id of the row they just read, so the write cannot land on a
    /// different org's connection even if the id were attacker-chosen.
    pub async fn store_tokens(
        &self,
        id: Uuid,
        org_id: Uuid,
        access: &[u8],
        refresh: Option<&[u8]>,
        expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        sqlx::query(
            "UPDATE broker_credentials
             SET access_token_ciphertext = $3,
                 refresh_token_ciphertext = COALESCE($4, refresh_token_ciphertext),
                 token_expires_at = $5,
                 state_token = NULL,
                 code_verifier = NULL,
                 state_expires_at = NULL
             WHERE id = $1 AND org_id = $2",
        )
        .bind(id)
        .bind(org_id)
        .bind(access)
        .bind(refresh)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .context("failed to store broker tokens")?;
        tx.commit()
            .await
            .context("failed to commit broker tokens")?;
        Ok(())
    }

    /// Org-scoped; see [`BrokerCredentialRepo::create_pending`].
    pub async fn delete(&self, user_id: Uuid, id: Uuid, org_id: Uuid) -> anyhow::Result<u64> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let rows = sqlx::query(
            "DELETE FROM broker_credentials
             WHERE user_id = $1 AND id = $2 AND org_id = $3",
        )
        .bind(user_id)
        .bind(id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .context("failed to delete broker credential")?
        .rows_affected();
        tx.commit()
            .await
            .context("failed to commit broker credential delete")?;
        Ok(rows)
    }
}
