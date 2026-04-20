pub mod nws;

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
/// Phase 4 only handles `ConnectorKind::RustNative` with name-based dispatch.
pub fn build_from_row(conn: &Connector) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    match conn.kind {
        ConnectorKind::RustNative => {
            let name_lower = conn.name.to_lowercase();
            if name_lower == "nws" || name_lower.starts_with("nws ") {
                let c = nws::NwsConnector::from_config(&conn.config)?;
                Ok(Box::new(c))
            } else {
                anyhow::bail!(
                    "unknown rust_native connector name '{}'; expected name starting with 'nws'",
                    conn.name
                )
            }
        }
        ConnectorKind::Mcp => {
            anyhow::bail!("MCP connectors not yet implemented (Phase 9)")
        }
        ConnectorKind::Openapi => {
            anyhow::bail!("OpenAPI connectors not yet implemented (Phase 9)")
        }
    }
}
