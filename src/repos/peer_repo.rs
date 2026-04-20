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
            "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy)
             VALUES ($1, $2, $3, $4)
             RETURNING id, name, mcp_url, issuer_id, sharing_policy, status, created_at",
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
            "SELECT id, name, mcp_url, issuer_id, sharing_policy, status, created_at
             FROM peers
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list peers")
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Peer>> {
        sqlx::query_as::<_, Peer>(
            "SELECT id, name, mcp_url, issuer_id, sharing_policy, status, created_at
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
             RETURNING id, name, mcp_url, issuer_id, sharing_policy, status, created_at",
        )
        .bind(id)
        .bind(status)
        .fetch_one(&self.pool)
        .await
        .context("failed to update peer status")
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
