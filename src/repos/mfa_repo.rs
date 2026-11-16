use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::rls::org_scoped_tx;

/// `mfa_enrollments` and `mfa_recovery_codes`. Every method runs inside
/// `rls::org_scoped_tx` for the caller's org: the org-isolation policies filter
/// reads and are also the insert check.
pub struct MfaRepo {
    pool: PgPool,
}

impl MfaRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Store a fresh TOTP secret. Re-enrolling replaces the secret and clears
    /// activation, so the user must confirm the new one.
    pub async fn upsert_enrollment(
        &self,
        org_id: Uuid,
        user_id: Uuid,
        ciphertext: &[u8],
    ) -> anyhow::Result<()> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        sqlx::query(
            "INSERT INTO mfa_enrollments (user_id, org_id, totp_secret_ciphertext)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id) DO UPDATE
             SET totp_secret_ciphertext = EXCLUDED.totp_secret_ciphertext, activated_at = NULL",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(ciphertext)
        .execute(&mut *tx)
        .await
        .context("failed to upsert mfa enrollment")?;
        tx.commit()
            .await
            .context("failed to commit mfa enrollment")?;
        Ok(())
    }

    pub async fn activate(&self, org_id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        sqlx::query("UPDATE mfa_enrollments SET activated_at = now() WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .context("failed to activate mfa enrollment")?;
        tx.commit()
            .await
            .context("failed to commit mfa activation")?;
        Ok(())
    }

    pub async fn totp_ciphertext(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let ciphertext = sqlx::query_scalar(
            "SELECT totp_secret_ciphertext FROM mfa_enrollments WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read totp secret")?;
        tx.commit()
            .await
            .context("failed to commit totp secret read")?;
        Ok(ciphertext)
    }

    /// True once the enrollment has been confirmed with a valid code.
    pub async fn is_enrolled(&self, org_id: Uuid, user_id: Uuid) -> anyhow::Result<bool> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let enrolled = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM mfa_enrollments WHERE user_id = $1 AND activated_at IS NOT NULL)",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to read mfa enrollment state")?;
        tx.commit()
            .await
            .context("failed to commit mfa enrollment state read")?;
        Ok(enrolled)
    }

    pub async fn delete_enrollment(&self, org_id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        sqlx::query("DELETE FROM mfa_enrollments WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .context("failed to delete mfa enrollment")?;
        tx.commit()
            .await
            .context("failed to commit mfa enrollment delete")?;
        Ok(())
    }

    pub async fn recovery_codes_viewed_at(
        &self,
        org_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<DateTime<Utc>>> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let viewed: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
            "SELECT recovery_codes_viewed_at FROM mfa_enrollments WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read recovery_codes_viewed_at")?;
        tx.commit()
            .await
            .context("failed to commit recovery_codes_viewed_at read")?;
        Ok(viewed.flatten())
    }

    /// Insert the hashed recovery codes and mark them viewed, in one
    /// transaction so a failure part way leaves neither.
    pub async fn store_recovery_codes(
        &self,
        org_id: Uuid,
        user_id: Uuid,
        code_hashes: &[String],
    ) -> anyhow::Result<()> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        for hash in code_hashes {
            sqlx::query(
                "INSERT INTO mfa_recovery_codes (user_id, org_id, code_hash) VALUES ($1, $2, $3)",
            )
            .bind(user_id)
            .bind(org_id)
            .bind(hash)
            .execute(&mut *tx)
            .await
            .context("failed to insert recovery code")?;
        }
        sqlx::query(
            "UPDATE mfa_enrollments SET recovery_codes_viewed_at = now() WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("failed to mark recovery codes viewed")?;
        tx.commit()
            .await
            .context("failed to commit recovery codes")?;
        Ok(())
    }

    pub async fn unused_recovery_count(&self, org_id: Uuid, user_id: Uuid) -> anyhow::Result<i64> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mfa_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to count recovery codes")?;
        tx.commit()
            .await
            .context("failed to commit recovery code count")?;
        Ok(count)
    }

    /// Mark one unused code with this hash as used. Returns whether one matched.
    pub async fn consume_recovery_code(
        &self,
        org_id: Uuid,
        user_id: Uuid,
        code_hash: &str,
    ) -> anyhow::Result<bool> {
        let mut tx = org_scoped_tx(&self.pool, org_id).await?;
        let row: Option<Uuid> = sqlx::query_scalar(
            "UPDATE mfa_recovery_codes SET used_at = now()
             WHERE id = (
                 SELECT id FROM mfa_recovery_codes
                 WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL
                 LIMIT 1
             )
             RETURNING id",
        )
        .bind(user_id)
        .bind(code_hash)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to consume recovery code")?;
        tx.commit()
            .await
            .context("failed to commit recovery code consume")?;
        Ok(row.is_some())
    }
}
