use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{models::WorkspacePeerCredential, util::token_crypto};

/// Non-secret columns. The ciphertext is never part of a metadata SELECT.
const CREDENTIAL_COLUMNS: &str =
    "id, org_id, workspace_id, peer_id, created_by, created_at, rotated_at";

pub struct WorkspacePeerCredentialRepo {
    pool: PgPool,
}

/// `rotated` distinguishes a first store from a replacement, for the audit verb.
pub struct CredentialUpsert {
    pub credential: WorkspacePeerCredential,
    pub rotated: bool,
}

impl WorkspacePeerCredentialRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Store or replace the credential for (workspace, peer). Replacement is
    /// rotation: it rewrites `credential_ciphertext` in place, so rotating is a
    /// config operation with no schema change.
    pub async fn upsert(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        plaintext: &str,
        created_by: Option<Uuid>,
    ) -> anyhow::Result<CredentialUpsert> {
        let ciphertext = token_crypto::encrypt_versioned(plaintext.as_bytes())
            .context("failed to encrypt peer credential")?;
        let credential = sqlx::query_as::<_, WorkspacePeerCredential>(&format!(
            "INSERT INTO workspace_peer_credentials
               (workspace_id, peer_id, credential_ciphertext, created_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT ON CONSTRAINT wpc_unique_workspace_peer DO UPDATE
               SET credential_ciphertext = EXCLUDED.credential_ciphertext,
                   rotated_at = now()
             RETURNING {CREDENTIAL_COLUMNS}"
        ))
        .bind(workspace_id)
        .bind(peer_id)
        .bind(&ciphertext)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert workspace peer credential")?;
        Ok(CredentialUpsert {
            rotated: credential.rotated_at.is_some(),
            credential,
        })
    }

    pub async fn get(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Option<WorkspacePeerCredential>> {
        sqlx::query_as::<_, WorkspacePeerCredential>(&format!(
            "SELECT {CREDENTIAL_COLUMNS} FROM workspace_peer_credentials
             WHERE workspace_id = $1 AND peer_id = $2 AND org_id = $3"
        ))
        .bind(workspace_id)
        .bind(peer_id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get workspace peer credential")
    }

    pub async fn list_for_workspace(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Vec<WorkspacePeerCredential>> {
        sqlx::query_as::<_, WorkspacePeerCredential>(&format!(
            "SELECT {CREDENTIAL_COLUMNS} FROM workspace_peer_credentials
             WHERE workspace_id = $1 AND org_id = $2
             ORDER BY created_at DESC"
        ))
        .bind(workspace_id)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workspace peer credentials")
    }

    pub async fn delete(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workspace_peer_credentials
             WHERE workspace_id = $1 AND peer_id = $2 AND org_id = $3",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(org_id)
        .execute(&self.pool)
        .await
        .context("failed to delete workspace peer credential")?;
        Ok(result.rows_affected() > 0)
    }

    /// Decrypt the outbound bearer credential for (workspace, peer). Only the
    /// outbound MCP auth path in `services::peer_tokens` calls this.
    pub async fn secret_for(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Option<String>> {
        let ciphertext: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT credential_ciphertext FROM workspace_peer_credentials
             WHERE workspace_id = $1 AND peer_id = $2",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to read workspace peer credential")?;
        let Some(ciphertext) = ciphertext else {
            return Ok(None);
        };
        let plaintext = token_crypto::decrypt_versioned(&ciphertext)
            .context("failed to decrypt peer credential")?;
        String::from_utf8(plaintext)
            .context("peer credential plaintext is not utf-8")
            .map(Some)
    }
}
