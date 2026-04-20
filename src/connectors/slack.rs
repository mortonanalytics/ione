use anyhow::Context;

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor};

pub struct SlackConnector {
    pub webhook_url: String,
    pub http: reqwest::Client,
}

impl SlackConnector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let webhook_url = config["webhook_url"]
            .as_str()
            .context("Slack connector config missing 'webhook_url'")?
            .to_string();
        Ok(Self {
            webhook_url,
            http: reqwest::Client::new(),
        })
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for SlackConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![])
    }

    async fn poll(
        &self,
        _stream_name: &str,
        _cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        Ok(PollResult {
            events: vec![],
            next_cursor: None,
        })
    }

    async fn invoke(&self, op: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        if op != "send" {
            anyhow::bail!("SlackConnector: unsupported op '{}'; expected 'send'", op);
        }

        let text = args["text"]
            .as_str()
            .context("Slack invoke 'send' requires args.text")?;

        let body = serde_json::json!({ "text": text });

        let resp = self
            .http
            .post(&self.webhook_url)
            .json(&body)
            .send()
            .await
            .context("Slack webhook HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Slack webhook returned non-2xx status {}: {}",
                status,
                body_text
            );
        }

        Ok(serde_json::json!({ "ok": true }))
    }
}
