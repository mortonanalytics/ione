pub mod firms;
pub mod fs_s3;
pub mod irwin;
pub mod mcp_client;
pub mod nws;
pub mod openapi;
pub mod slack;
pub mod smtp;
pub mod validate;

use crate::models::{Connector, ConnectorKind};

/// Describes a stream that a connector exposes by default.
pub struct StreamDescriptor {
    pub name: String,
    pub schema: serde_json::Value,
}

/// Input for inserting a new stream event.
pub struct StreamEventInput {
    pub payload: serde_json::Value,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

/// Result of a poll operation.
pub struct PollResult {
    pub events: Vec<StreamEventInput>,
    pub next_cursor: Option<serde_json::Value>,
}

/// Trait implemented by every connector kind.
#[async_trait::async_trait]
pub trait ConnectorImpl: Send + Sync {
    fn kind(&self) -> ConnectorKind;

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>>;

    async fn poll(
        &self,
        stream_name: &str,
        cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult>;

    async fn invoke(
        &self,
        _op: &str,
        _args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("invoke not implemented for this connector")
    }
}

/// Build a boxed connector implementation from a `Connector` DB row.
///
/// Dispatch priority:
/// 1. config["kind"] field if present (explicit kind hint).
/// 2. name prefix matching (case-insensitive): "slack*" → Slack, "smtp*" → SMTP, "nws*" → NWS.
pub fn build_from_row(conn: &Connector) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    build(conn.kind.clone(), &conn.name, &conn.config)
}

/// Build a connector implementation from request/DB fields.
pub fn build(kind: ConnectorKind, name: &str, config: &serde_json::Value) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    match kind {
        ConnectorKind::RustNative => {
            // Check for an explicit kind hint in config first.
            let kind_hint = config["kind"].as_str().unwrap_or("").to_lowercase();
            let name_lower = name.to_lowercase();

            if kind_hint == "slack" || name_lower.starts_with("slack") {
                let c = slack::SlackConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            if kind_hint == "smtp" || name_lower.starts_with("smtp") {
                let c = smtp::SmtpConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            if kind_hint == "nws" || name_lower == "nws" || name_lower.starts_with("nws ") {
                let c = nws::NwsConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            if kind_hint == "firms" || name_lower.starts_with("firms") {
                let c = firms::FirmsConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            if kind_hint == "fs_s3"
                || kind_hint == "s3"
                || kind_hint == "fs"
                || name_lower.starts_with("s3")
                || name_lower.starts_with("fs_s3")
                || name_lower.starts_with("documents")
            {
                let c = fs_s3::FsS3Connector::from_config(config)?;
                return Ok(Box::new(c));
            }

            if kind_hint == "irwin" || name_lower.starts_with("irwin") {
                let c = irwin::IrwinConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            anyhow::bail!(
                "unknown rust_native connector name '{}'; set config.kind to 'slack', 'smtp', 'nws', 'firms', 'fs_s3', or 'irwin'",
                name
            )
        }
        ConnectorKind::Mcp => {
            let c = mcp_client::McpClientConnector::from_config(config)?;
            Ok(Box::new(c))
        }
        ConnectorKind::Openapi => {
            let c = openapi::OpenApiConnector::from_config(config)?;
            Ok(Box::new(c))
        }
    }
}
