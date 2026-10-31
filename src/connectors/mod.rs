pub mod firms;
pub mod fs_s3;
pub mod geojson_poll;
pub mod irwin;
pub mod mcp_client;
pub mod nws;
pub mod openapi;
pub mod peer_session;
pub mod slack;
pub mod smtp;
pub mod validate;

use std::time::Duration;

use sqlx::PgPool;

use crate::models::{Connector, ConnectorKind};

/// Request bound for the built-in FIRMS, IRWIN, NWS and Slack connectors. The
/// scheduler bounds its own polls, but the "poll now" and connector-create
/// routes call `poll` directly, so without this a silent upstream hangs them.
pub(crate) const CONNECTOR_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// An HTTP client with a whole-request timeout and a connect timeout of at most
/// 10 s. Falls back to an unbounded client rather than panicking if the builder
/// fails.
pub(crate) fn connector_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Describes a stream that a connector exposes by default.
pub struct StreamDescriptor {
    pub name: String,
    pub schema: serde_json::Value,
    /// Optional geometry + style mapping persisted to `streams.view_config`.
    /// `None` means the stream is not rendered as a point layer.
    pub view_config: Option<serde_json::Value>,
}

/// Input for inserting a new stream event.
pub struct StreamEventInput {
    pub payload: serde_json::Value,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub dedup_key: Option<String>,
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

    /// Whether this connector implements `invoke`. Delivery treats approval of
    /// an artifact on a non-invokable connector as terminal (decision is the
    /// outcome), not a delivery failure.
    fn supports_invoke(&self) -> bool {
        false
    }
}

/// Build a boxed connector implementation from a `Connector` DB row.
///
/// Dispatch priority:
/// 1. config["kind"] field if present (explicit kind hint).
/// 2. name prefix matching (case-insensitive): "slack*" → Slack, "smtp*" → SMTP, "nws*" → NWS.
pub fn build_from_row(conn: &Connector) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    build_with_pool(conn.kind.clone(), &conn.name, &conn.config, None)
}

pub fn build_from_row_with_pool(
    conn: &Connector,
    pool: PgPool,
) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    build_with_pool(conn.kind.clone(), &conn.name, &conn.config, Some(pool))
}

/// Build a connector implementation from request/DB fields.
pub fn build(
    kind: ConnectorKind,
    name: &str,
    config: &serde_json::Value,
) -> anyhow::Result<Box<dyn ConnectorImpl>> {
    build_with_pool(kind, name, config, None)
}

pub fn build_with_pool(
    kind: ConnectorKind,
    name: &str,
    config: &serde_json::Value,
    pool: Option<PgPool>,
) -> anyhow::Result<Box<dyn ConnectorImpl>> {
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

            if kind_hint == "geojson_poll" || name_lower.starts_with("geojson") {
                let c = geojson_poll::GeoJsonPollConnector::from_config(config)?;
                return Ok(Box::new(c));
            }

            anyhow::bail!(
                "unknown rust_native connector name '{}'; set config.kind to 'slack', 'smtp', 'nws', 'firms', 'fs_s3', 'irwin', or 'geojson_poll'",
                name
            )
        }
        ConnectorKind::Mcp => {
            let c = mcp_client::McpClientConnector::from_config(config, pool)?;
            Ok(Box::new(c))
        }
        ConnectorKind::Openapi => {
            let c = openapi::OpenApiConnector::from_config(config)?;
            Ok(Box::new(c))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connector_http_client_times_out_against_a_silent_upstream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        // Accept and hold connections open without ever writing a byte.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });

        let client = connector_http_client(Duration::from_millis(300));
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("http://{addr}/")).send(),
        )
        .await
        .expect("the client's own timeout must fire well before 5 s");
        let err = result.expect_err("a silent upstream must not produce a response");
        assert!(err.is_timeout(), "expected a timeout error, got {err:?}");
    }
}
