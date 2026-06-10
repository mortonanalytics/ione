use std::{collections::HashSet, net::IpAddr};

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{Connector, ConnectorKind, Peer};
use crate::repos::{ConnectorRepo, PeerRepo, TrustIssuerRepo};

/// Register a new peer. Validates that the issuer_id exists before inserting.
pub async fn register_peer(
    pool: &PgPool,
    org_id: Uuid,
    name: &str,
    mcp_url: &str,
    issuer_id: Uuid,
    sharing_policy: serde_json::Value,
) -> anyhow::Result<Peer> {
    validate_mcp_url_syntax(mcp_url)?;
    validate_name(name)?;

    let issuer_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM trust_issuers WHERE id = $1 AND org_id = $2)",
    )
    .bind(issuer_id)
    .bind(org_id)
    .fetch_one(pool)
    .await
    .context("failed to check issuer existence")?;

    if !issuer_exists {
        anyhow::bail!("issuer_id '{}' not found in caller org", issuer_id);
    }

    validate_mcp_url(mcp_url).await?;

    let repo = PeerRepo::new(pool.clone());
    let peer = repo
        .insert(name, mcp_url, issuer_id, sharing_policy)
        .await?;
    let prefix = derive_prefix_for_org(pool, org_id, name).await?;
    repo.set_tool_prefix(peer.id, &prefix).await?;
    repo.get(peer.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("inserted peer disappeared"))
}

/// When a workspace wants to consume a peer, create a ConnectorKind::Mcp row in that
/// workspace pointing at the peer's MCP URL.
pub async fn auto_create_connector_for_peer(
    pool: &PgPool,
    workspace_id: Uuid,
    peer: &Peer,
) -> anyhow::Result<Connector> {
    let connector_repo = ConnectorRepo::new(pool.clone());

    let config = serde_json::json!({
        "mcp_url": peer.mcp_url,
        "peer_id": peer.id,
        "workspace_id": workspace_id,
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

pub async fn validate_mcp_url(url: &str) -> anyhow::Result<()> {
    validate_mcp_url_syntax(url)?;
    if private_peers_allowed() && private_peer_allowlisted(url)? {
        let parsed = crate::util::url_guard::parse_and_validate_url(url, "mcp_url")?;
        if parsed.host_str().is_some() {
            return Ok(());
        }
    }
    crate::util::safe_http::ensure_public_url(url)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(())
}

fn validate_mcp_url_syntax(url: &str) -> anyhow::Result<()> {
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

fn private_peers_allowed() -> bool {
    std::env::var("IONE_ALLOW_PRIVATE_PEERS")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn private_peer_allowlisted(raw: &str) -> anyhow::Result<bool> {
    let url = url::Url::parse(raw).context("invalid mcp_url")?;
    let Some(host) = url.host_str() else {
        return Ok(false);
    };
    let entries: Vec<String> = std::env::var("IONE_PRIVATE_PEER_ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if entries.is_empty() {
        return Ok(false);
    }
    let host_lower = host.trim_matches(['[', ']']).to_ascii_lowercase();
    for entry in entries {
        if entry == host_lower || (entry.starts_with('.') && host_lower.ends_with(&entry)) {
            return Ok(true);
        }
        if cidr_contains(&entry, &host_lower) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cidr_contains(cidr: &str, host: &str) -> bool {
    let Some((network, bits)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix_bits) = bits.parse::<u32>() else {
        return false;
    };
    let Ok(network_ip) = network.parse::<IpAddr>() else {
        return false;
    };
    let Ok(host_ip) = host.parse::<IpAddr>() else {
        return false;
    };
    match (network_ip, host_ip) {
        (IpAddr::V4(network), IpAddr::V4(host)) if prefix_bits <= 32 => {
            let mask = if prefix_bits == 0 {
                0
            } else {
                u32::MAX << (32 - prefix_bits)
            };
            (u32::from(network) & mask) == (u32::from(host) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(host)) if prefix_bits <= 128 => {
            let mask = if prefix_bits == 0 {
                0
            } else {
                u128::MAX << (128 - prefix_bits)
            };
            (u128::from(network) & mask) == (u128::from(host) & mask)
        }
        _ => false,
    }
}

async fn derive_prefix_for_org(pool: &PgPool, org_id: Uuid, name: &str) -> anyhow::Result<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT tool_prefix FROM peers WHERE org_id = $1 AND tool_prefix IS NOT NULL",
    )
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("failed to list peer tool prefixes")?;
    let taken: HashSet<String> = rows.into_iter().collect();
    Ok(crate::services::federation::derive_prefix(name, &taken))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[test]
    fn cidr_allowlist_matches_literal_private_ips() {
        assert!(cidr_contains("10.0.0.0/8", "10.2.3.4"));
        assert!(!cidr_contains("10.0.0.0/8", "192.168.1.4"));
        assert!(cidr_contains("fd00::/8", "fd00::1"));
    }

    #[tokio::test]
    async fn validate_mcp_url_rejects_link_local_even_when_private_peers_allowed() {
        let _guard = env_lock().lock().await;
        let old_allow = std::env::var("IONE_ALLOW_PRIVATE_PEERS").ok();
        let old_list = std::env::var("IONE_PRIVATE_PEER_ALLOWLIST").ok();
        std::env::set_var("IONE_ALLOW_PRIVATE_PEERS", "1");
        std::env::set_var("IONE_PRIVATE_PEER_ALLOWLIST", "169.254.0.0/16");
        let result = validate_mcp_url("http://169.254.169.254/mcp").await;
        if let Some(value) = old_allow {
            std::env::set_var("IONE_ALLOW_PRIVATE_PEERS", value);
        } else {
            std::env::remove_var("IONE_ALLOW_PRIVATE_PEERS");
        }
        if let Some(value) = old_list {
            std::env::set_var("IONE_PRIVATE_PEER_ALLOWLIST", value);
        } else {
            std::env::remove_var("IONE_PRIVATE_PEER_ALLOWLIST");
        }
        assert!(result.is_err());
    }
}
