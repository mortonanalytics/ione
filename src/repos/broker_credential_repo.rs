use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::BrokerCredential;

pub struct BrokerCredentialRepo {
    pool: PgPool,
}

impl BrokerCredentialRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

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
        sqlx::query_as::<_, BrokerCredential>(
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
        .fetch_one(&self.pool)
        .await
        .context("failed to create pending broker credential")
    }

    pub async fn list_for_user(&self, user_id: Uuid) -> anyhow::Result<Vec<BrokerCredential>> {
        sqlx::query_as::<_, BrokerCredential>(
            "SELECT id, user_id, org_id, provider, label, scopes, access_token_ciphertext,
                refresh_token_ciphertext, token_expires_at, state_token, code_verifier,
                state_expires_at, created_at
             FROM broker_credentials
             WHERE user_id = $1
             ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list broker credentials")
    }

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

    pub async fn store_tokens(
        &self,
        id: Uuid,
        access: &[u8],
        refresh: Option<&[u8]>,
        expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE broker_credentials
             SET access_token_ciphertext = $2,
                 refresh_token_ciphertext = COALESCE($3, refresh_token_ciphertext),
                 token_expires_at = $4,
                 state_token = NULL,
                 code_verifier = NULL,
                 state_expires_at = NULL
             WHERE id = $1",
        )
        .bind(id)
        .bind(access)
        .bind(refresh)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .context("failed to store broker tokens")?;
        Ok(())
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> anyhow::Result<u64> {
        let rows = sqlx::query("DELETE FROM broker_credentials WHERE user_id = $1 AND id = $2")
            .bind(user_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete broker credential")?
            .rows_affected();
        Ok(rows)
    }
}
