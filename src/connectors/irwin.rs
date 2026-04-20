/// IRWIN (Integrated Reporting of Wildland-fire Information) read connector.
///
/// Polls the IRWIN incidents endpoint and emits one `StreamEventInput` per
/// incident in the response.
///
/// Config shape:
/// ```json
/// {
///   "base_url": "https://irwin.doi.gov/arcgis/rest/services",
///   "api_key": "optional bearer token"
/// }
/// ```
///
/// Mock mode: if `base_url` starts with `mock://`, the connector returns incidents
/// from `infra/fixtures/irwin_incidents.json` instead of making a network call.
/// This makes the demo runnable without IRWIN credentials.
///
/// To swap in real IRWIN credentials:
///   1. Set `base_url` to `https://irwin.doi.gov/arcgis/rest/services` (or your
///      agency's IRWIN gateway URL).
///   2. Set `api_key` to your bearer token or leave empty for unauthenticated
///      access (if the endpoint allows it in your environment).
///   3. The cursor (`modified_since`) is passed as `?modified_since=<ISO8601>` on
///      incremental polls; the first poll omits it.
use anyhow::Context;
use serde_json::json;

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

pub struct IrwinConnector {
    pub base_url: String,
    pub api_key: Option<String>,
    pub http: reqwest::Client,
    pub mock_mode: bool,
    pub fixture_path: String,
}

impl IrwinConnector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let base_url = config["base_url"]
            .as_str()
            .context("IRWIN connector config missing 'base_url'")?
            .to_string();

        let mock_mode = base_url.starts_with("mock://");
        let api_key = config["api_key"].as_str().map(str::to_string);

        let fixture_path = config["fixture_path"]
            .as_str()
            .unwrap_or("infra/fixtures/irwin_incidents.json")
            .to_string();

        Ok(Self {
            base_url,
            api_key,
            http: reqwest::Client::new(),
            mock_mode,
            fixture_path,
        })
    }

    async fn fetch_incidents(
        &self,
        modified_since: Option<&str>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        if self.mock_mode {
            return read_mock_incidents(&self.fixture_path);
        }

        let mut url = format!("{}/incidents", self.base_url);
        if let Some(since) = modified_since {
            url.push_str(&format!("?modified_since={}", urlencoding_encode(since)));
        }

        let mut req = self.http.get(&url);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await.context("IRWIN HTTP request failed")?;

        if !resp.status().is_success() {
            anyhow::bail!("IRWIN API returned status {}", resp.status());
        }

        let data: serde_json::Value = resp.json().await.context("IRWIN response parse failed")?;

        // IRWIN may return an array directly or wrap in { "incidents": [...] }.
        let incidents = if data.is_array() {
            data.as_array().cloned().unwrap_or_default()
        } else {
            data["incidents"].as_array().cloned().unwrap_or_default()
        };

        Ok(incidents)
    }
}

fn read_mock_incidents(path: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let content = std::fs::read_to_string(path)
        .or_else(|_| {
            let manifest = env!("CARGO_MANIFEST_DIR");
            let full = format!("{}/{}", manifest, path);
            std::fs::read_to_string(&full)
        })
        .context("failed to read IRWIN fixture JSON")?;

    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&content).context("IRWIN fixture is not a JSON array")?;

    Ok(arr)
}

/// Minimal percent-encoding for the modified_since query parameter value.
fn urlencoding_encode(s: &str) -> String {
    s.replace(':', "%3A").replace('+', "%2B")
}

fn incident_to_event(incident: serde_json::Value) -> anyhow::Result<StreamEventInput> {
    // Use ModifiedBySystem as observed_at; fall back to FireDiscoveryDateTime then now.
    let ts_str = incident["ModifiedBySystem"]
        .as_str()
        .or_else(|| incident["FireDiscoveryDateTime"].as_str());

    let observed_at = if let Some(s) = ts_str {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now())
    } else {
        chrono::Utc::now()
    };

    Ok(StreamEventInput {
        payload: incident,
        observed_at,
    })
}

#[async_trait::async_trait]
impl ConnectorImpl for IrwinConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![StreamDescriptor {
            name: "incidents".to_string(),
            schema: json!({
                "type": "object",
                "description": "IRWIN wildland fire incident records"
            }),
        }])
    }

    async fn poll(
        &self,
        stream_name: &str,
        cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        if stream_name != "incidents" {
            anyhow::bail!(
                "IRWIN connector only supports stream 'incidents', got '{}'",
                stream_name
            );
        }

        let modified_since = cursor
            .as_ref()
            .and_then(|c| c["modified_since"].as_str())
            .map(str::to_string);

        let incidents = self.fetch_incidents(modified_since.as_deref()).await?;

        // Use the most recent ModifiedBySystem as the next cursor.
        let max_ts = incidents
            .iter()
            .filter_map(|i| i["ModifiedBySystem"].as_str())
            .max()
            .map(|s| json!({ "modified_since": s }));

        let mut events = Vec::with_capacity(incidents.len());
        for inc in incidents {
            events.push(incident_to_event(inc)?);
        }

        Ok(PollResult {
            events,
            next_cursor: max_ts,
        })
    }
}
