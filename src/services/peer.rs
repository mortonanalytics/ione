use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Connector, ConnectorKind, Peer};
use crate::repos::{ConnectorRepo, PeerRepo, TrustIssuerRepo};

/// Register a new peer. Validates that the issuer_id exists before inserting.
pub async fn register_peer(
    pool: &PgPool,
    name: &str,
    mcp_url: &str,
    issuer_id: Uuid,
    sharing_policy: serde_json::Value,
) -> anyhow::Result<Peer> {
    validate_mcp_url(mcp_url)?;
    validate_name(name)?;

    // Validate issuer exists. We look across all orgs since peers are global.
    let issuer_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trust_issuers WHERE id = $1)")
            .bind(issuer_id)
            .fetch_one(pool)
            .await
            .context("failed to check issuer existence")?;

    if !issuer_exists {
        anyhow::bail!("issuer_id '{}' not found in trust_issuers", issuer_id);
    }

    let repo = PeerRepo::new(pool.clone());
    repo.insert(name, mcp_url, issuer_id, sharing_policy).await
}

/// When a workspace wants to consume a peer, create a ConnectorKind::Mcp row in that
/// workspace pointing at the peer's MCP URL.
pub async fn auto_create_connector_for_peer(
    pool: &PgPool,
    workspace_id: Uuid,
    peer: &Peer,
) -> anyhow::Result<Connector> {
    let connector_repo = ConnectorRepo::new(pool.clone());

    let bearer_token = if let Some(ciphertext) = peer.access_token_ciphertext.as_deref() {
        crate::util::token_crypto::decrypt_token(ciphertext)
            .context("failed to decrypt peer access token")?
    } else {
        std::env::var("IONE_OAUTH_STATIC_BEARER").unwrap_or_default()
    };

    let config = serde_json::json!({
        "mcp_url": peer.mcp_url,
        "bearer_token": bearer_token,
        "peer_id": peer.id,
    });

    connector_repo
        .create(
            workspace_id,
            ConnectorKind::Mcp,
            &format!("peer:{}", peer.name),
            config,
        )
        .await
        .context("failed to create mcp connector for peer")
}

/// Check the peer's sharing_policy to decide whether this severity is allowed.
/// Minimal v1 schema: `{ "allow_severity": ["routine","flagged","command"], "allow_workspaces": [uuid…] | "*" }`
pub fn check_sharing_policy(
    policy: &serde_json::Value,
    severity: &str,
    workspace_id: Uuid,
) -> PolicyDecision {
    // allow_severity check
    if let Some(arr) = policy["allow_severity"].as_array() {
        let allowed: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !allowed.contains(&severity) {
            return PolicyDecision::Blocked(format!(
                "severity '{}' not in allow_severity {:?}",
                severity, allowed
            ));
        }
    }
    // Empty policy object → no restrictions (allow all)

    // allow_workspaces check
    match policy["allow_workspaces"].as_str() {
        Some("*") | None => {}
        Some(_) => {
            // Array case
            if let Some(arr) = policy["allow_workspaces"].as_array() {
                let allowed_ids: Vec<Uuid> = arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
                if !allowed_ids.contains(&workspace_id) {
                    return PolicyDecision::Blocked(format!(
                        "workspace {} not in allow_workspaces",
                        workspace_id
                    ));
                }
            }
        }
    }

    PolicyDecision::Allow
}

pub enum PolicyDecision {
    Allow,
    Blocked(String),
}

fn validate_mcp_url(url: &str) -> anyhow::Result<()> {
    if url.is_empty() {
        anyhow::bail!("mcp_url must not be empty");
    }
    if url.len() > 2048 {
        anyhow::bail!("mcp_url exceeds 2048 character limit");
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("mcp_url must start with http:// or https://");
    }
    Ok(())
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("name must not be empty");
    }
    if name.len() > 255 {
        anyhow::bail!("name exceeds 255 character limit");
    }
    Ok(())
}

pub fn issuer_repo(pool: PgPool) -> TrustIssuerRepo {
    TrustIssuerRepo::new(pool)
}
