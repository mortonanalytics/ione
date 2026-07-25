use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::WorkspacePeerDelegation;

/// Non-secret columns. Neither ciphertext is ever part of a metadata SELECT.
const DELEGATION_COLUMNS: &str = "id, org_id, workspace_id, peer_id, granted_by,
     token_expires_at, created_at, refreshed_at";

/// `DELETE ... RETURNING` shape for a consumed pending authorization.
type PendingRow = (Uuid, Uuid, String, String, String, String, Option<Uuid>);

/// SELECT shape for the encrypted delegated token material.
type MaterialRow = (
    Uuid,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<DateTime<Utc>>,
    String,
    String,
);

/// The encrypted token material for one (workspace, peer) delegation. Read only
/// by the outbound MCP auth path; never serialized.
pub struct DelegatedTokenMaterial {
    pub id: Uuid,
    pub access_token_ciphertext: Vec<u8>,
    pub refresh_token_ciphertext: Option<Vec<u8>>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub oauth_client_id: String,
    pub token_endpoint: String,
}

/// One in-flight delegation authorization, consumed exactly once by the
/// callback.
pub struct PendingDelegation {
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub code_verifier: String,
    pub oauth_client_id: String,
    pub redirect_uri: String,
    pub token_endpoint: String,
    pub granted_by: Option<Uuid>,
}

pub struct WorkspacePeerDelegationRepo {
    pool: PgPool,
}

impl WorkspacePeerDelegationRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record an in-flight authorization. The nonce travels as the OAuth `state`
    /// parameter and is the only thing that identifies the flow on return.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_pending(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        nonce: &str,
        code_verifier: &str,
        oauth_client_id: &str,
        redirect_uri: &str,
        token_endpoint: &str,
        granted_by: Option<Uuid>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO workspace_peer_delegation_pending
                (workspace_id, peer_id, nonce, code_verifier, oauth_client_id,
                 redirect_uri, token_endpoint, granted_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + interval '10 minutes')",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(nonce)
        .bind(code_verifier)
        .bind(oauth_client_id)
        .bind(redirect_uri)
        .bind(token_endpoint)
        .bind(granted_by)
        .execute(&self.pool)
        .await
        .context("failed to record pending peer delegation")?;
        Ok(())
    }

    /// Single-use consumption: `DELETE ... RETURNING` so a replayed state finds
    /// nothing, and an expired row is never returned.
    pub async fn consume_pending(&self, nonce: &str) -> anyhow::Result<Option<PendingDelegation>> {
        let row: Option<PendingRow> = sqlx::query_as(
            "DELETE FROM workspace_peer_delegation_pending
                 WHERE nonce = $1 AND expires_at > now()
                 RETURNING workspace_id, peer_id, code_verifier, oauth_client_id,
                           redirect_uri, token_endpoint, granted_by",
        )
        .bind(nonce)
        .fetch_optional(&self.pool)
        .await
        .context("failed to consume pending peer delegation")?;
        Ok(row.map(
            |(
                workspace_id,
                peer_id,
                code_verifier,
                oauth_client_id,
                redirect_uri,
                token_endpoint,
                granted_by,
            )| PendingDelegation {
                workspace_id,
                peer_id,
                code_verifier,
                oauth_client_id,
                redirect_uri,
                token_endpoint,
                granted_by,
            },
        ))
    }

    /// Store or replace the delegated token for (workspace, peer). A repeat of
    /// the authorization dance rewrites the ciphertext in place, so re-consent
    /// never accumulates rows.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_tokens(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        oauth_client_id: &str,
        token_endpoint: &str,
        access_ciphertext: &[u8],
        refresh_ciphertext: Option<&[u8]>,
        token_expires_at: Option<DateTime<Utc>>,
        granted_by: Option<Uuid>,
    ) -> anyhow::Result<WorkspacePeerDelegation> {
        sqlx::query_as::<_, WorkspacePeerDelegation>(&format!(
            "INSERT INTO workspace_peer_delegations
               (workspace_id, peer_id, oauth_client_id, token_endpoint,
                access_token_ciphertext, refresh_token_ciphertext,
                token_expires_at, granted_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT ON CONSTRAINT wpd_unique_workspace_peer DO UPDATE
               SET oauth_client_id = EXCLUDED.oauth_client_id,
                   token_endpoint = EXCLUDED.token_endpoint,
                   access_token_ciphertext = EXCLUDED.access_token_ciphertext,
                   refresh_token_ciphertext = EXCLUDED.refresh_token_ciphertext,
                   token_expires_at = EXCLUDED.token_expires_at,
                   granted_by = EXCLUDED.granted_by,
                   refreshed_at = now()
             RETURNING {DELEGATION_COLUMNS}"
        ))
        .bind(workspace_id)
        .bind(peer_id)
        .bind(oauth_client_id)
        .bind(token_endpoint)
        .bind(access_ciphertext)
        .bind(refresh_ciphertext)
        .bind(token_expires_at)
        .bind(granted_by)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert workspace peer delegation")
    }

    /// Replace only the token material, after a refresh-grant exchange.
    pub async fn update_refreshed(
        &self,
        id: Uuid,
        access_ciphertext: &[u8],
        refresh_ciphertext: Option<&[u8]>,
        token_expires_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE workspace_peer_delegations
             SET access_token_ciphertext = $2,
                 refresh_token_ciphertext = COALESCE($3, refresh_token_ciphertext),
                 token_expires_at = $4,
                 refreshed_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(access_ciphertext)
        .bind(refresh_ciphertext)
        .bind(token_expires_at)
        .execute(&self.pool)
        .await
        .context("failed to update refreshed peer delegation")?;
        Ok(())
    }

    pub async fn get(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<Option<WorkspacePeerDelegation>> {
        sqlx::query_as::<_, WorkspacePeerDelegation>(&format!(
            "SELECT {DELEGATION_COLUMNS} FROM workspace_peer_delegations
             WHERE workspace_id = $1 AND peer_id = $2 AND org_id = $3"
        ))
        .bind(workspace_id)
        .bind(peer_id)
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get workspace peer delegation")
    }

    pub async fn delete(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
        org_id: Uuid,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workspace_peer_delegations
             WHERE workspace_id = $1 AND peer_id = $2 AND org_id = $3",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .bind(org_id)
        .execute(&self.pool)
        .await
        .context("failed to delete workspace peer delegation")?;
        Ok(result.rows_affected() > 0)
    }

    /// The encrypted delegated token for (workspace, peer). Only the outbound
    /// MCP auth path in `services::peer_tokens` calls this.
    pub async fn material_for(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Option<DelegatedTokenMaterial>> {
        let row: Option<MaterialRow> = sqlx::query_as(
            "SELECT id, access_token_ciphertext, refresh_token_ciphertext,
                        token_expires_at, oauth_client_id, token_endpoint
                 FROM workspace_peer_delegations
                 WHERE workspace_id = $1 AND peer_id = $2",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to read workspace peer delegation")?;
        Ok(row.map(
            |(
                id,
                access_token_ciphertext,
                refresh_token_ciphertext,
                token_expires_at,
                oauth_client_id,
                token_endpoint,
            )| DelegatedTokenMaterial {
                id,
                access_token_ciphertext,
                refresh_token_ciphertext,
                token_expires_at,
                oauth_client_id,
                token_endpoint,
            },
        ))
    }
}
