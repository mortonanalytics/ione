use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Peer, PeerStatus};

pub struct PeerRepo {
    pub pool: PgPool,
}

impl PeerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        name: &str,
        mcp_url: &str,
        issuer_id: Uuid,
        sharing_policy: serde_json::Value,
    ) -> anyhow::Result<Peer> {
        sqlx::query_as::<_, Peer>(
            "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy)
             SELECT org_id, $1, $2, id, $4 FROM trust_issuers WHERE id = $3
             RETURNING id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at,
                 oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext,
                 token_expires_at, tool_allowlist",
        )
        .bind(name)
        .bind(mcp_url)
        .bind(issuer_id)
        .bind(sharing_policy)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert peer")
    }

    pub async fn list(&self) -> anyhow::Result<Vec<Peer>> {
        sqlx::query_as::<_, Peer>(
            "SELECT id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at,
                 oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext,
                 token_expires_at, tool_allowlist
             FROM peers
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list peers")
    }

    pub async fn list_for_org(&self, org_id: Uuid) -> anyhow::Result<Vec<Peer>> {
        sqlx::query_as::<_, Peer>(
            "SELECT id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at,
                 oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext,
                 token_expires_at, tool_allowlist
             FROM peers
             WHERE org_id = $1
             ORDER BY created_at DESC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list peers by org")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Peer>> {
        sqlx::query_as::<_, Peer>(
            "SELECT id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at,
                 oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext,
                 token_expires_at, tool_allowlist
             FROM peers
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get peer")
    }

    pub async fn update_status(&self, id: Uuid, status: PeerStatus) -> anyhow::Result<Peer> {
        sqlx::query_as::<_, Peer>(
            "UPDATE peers
             SET status = $2
             WHERE id = $1
             RETURNING id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at,
                 oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext,
                 token_expires_at, tool_allowlist",
        )
        .bind(id)
        .bind(status)
        .fetch_one(&self.pool)
        .await
        .context("failed to update peer status")
    }

    pub async fn begin_oauth(&self, peer_id: Uuid, oauth_client_id: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE peers
             SET oauth_client_id = $1,
                 status = 'pending_oauth'
             WHERE id = $2",
        )
        .bind(oauth_client_id)
        .bind(peer_id)
        .execute(&self.pool)
        .await
        .context("failed to begin peer oauth")?;
        Ok(())
    }

    pub async fn set_tokens(
        &self,
        peer_id: Uuid,
        access_token_hash: &str,
        refresh_token_hash: &str,
        access_token_ciphertext: &[u8],
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE peers
             SET access_token_hash = $1,
                 refresh_token_hash = $2,
                 access_token_ciphertext = $3,
                 token_expires_at = $4,
                 status = 'pending_allowlist'
             WHERE id = $5",
        )
        .bind(access_token_hash)
        .bind(refresh_token_hash)
        .bind(access_token_ciphertext)
        .bind(expires_at)
        .bind(peer_id)
        .execute(&self.pool)
        .await
        .context("failed to set peer oauth tokens")?;
        Ok(())
    }

    pub async fn set_allowlist(
        &self,
        peer_id: Uuid,
        tool_allowlist: &serde_json::Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE peers
             SET tool_allowlist = $1,
                 status = 'active'
             WHERE id = $2",
        )
        .bind(tool_allowlist)
        .bind(peer_id)
        .execute(&self.pool)
        .await
        .context("failed to set peer tool allowlist")?;
        Ok(())
    }

    pub async fn set_status(&self, peer_id: Uuid, status: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE peers SET status = $1::peer_status WHERE id = $2")
            .bind(status)
            .bind(peer_id)
            .execute(&self.pool)
            .await
            .context("failed to set peer status")?;
        Ok(())
    }

    pub async fn get_tool_allowlist(&self, peer_id: Uuid) -> anyhow::Result<Vec<String>> {
        let val: serde_json::Value =
            sqlx::query_scalar("SELECT tool_allowlist FROM peers WHERE id = $1")
                .bind(peer_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to get peer tool allowlist")?;

        Ok(val
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    /// Find the mcp connector in a workspace that points at the given peer's mcp_url.
    pub async fn find_mcp_connector_for_peer(
        &self,
        workspace_id: Uuid,
        peer_id: Uuid,
    ) -> anyhow::Result<Option<Uuid>> {
        // Join peers to connectors via config.mcp_url matching peer.mcp_url.
        let connector_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT c.id
             FROM connectors c
             JOIN peers p ON p.mcp_url = c.config->>'mcp_url'
             WHERE c.workspace_id = $1
               AND p.id = $2
               AND c.kind = 'mcp'::connector_kind
             LIMIT 1",
        )
        .bind(workspace_id)
        .bind(peer_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find mcp connector for peer")?;

        Ok(connector_id)
    }
}
