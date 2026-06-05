use std::collections::HashMap;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::{
    models::ConnectorKind,
    services::event_layers::validate_view_config,
    util::{json_pointer::validate_json_pointer, url_guard},
};

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

pub(crate) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_ITEMS: usize = 10_000;
const MAX_ITEMS_LIMIT: usize = 10_000;
const MAX_DEDUP_KEY_LEN: usize = 512;

#[derive(Debug)]
pub struct GeoJsonPollConnector {
    config: GeoJsonPollConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeoJsonPollConfig {
    kind: String,
    feed_url: String,
    stream_name: String,
    #[serde(default = "default_items_pointer")]
    items_pointer: String,
    #[serde(default)]
    observed_at_pointer: Option<String>,
    observed_at_format: ObservedAtFormat,
    #[serde(default)]
    dedup_pointer: Option<String>,
    #[serde(default)]
    type_filter: Option<TypeFilterConfig>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    view_config: Option<Value>,
    #[serde(default)]
    field_types: Option<HashMap<String, FieldType>>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Number,
    String,
    Boolean,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeFilterConfig {
    pointer: String,
    allow: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ObservedAtFormat {
    Rfc3339,
    EpochMs,
    EpochS,
    None,
}

impl GeoJsonPollConnector {
    pub fn from_config(config: &Value) -> anyhow::Result<Self> {
        let parsed: GeoJsonPollConfig =
            serde_json::from_value(config.clone()).context("invalid geojson_poll config")?;
        parsed.validate()?;
        Ok(Self { config: parsed })
    }

    pub(crate) async fn fetch_feed_json(&self) -> anyhow::Result<Value> {
        let url = url_guard::parse_and_validate_url(&self.config.feed_url, "feed_url")?;
        let client = url_guard::guarded_client(self.timeout_ms());
        let resp = client
            .get(url.clone())
            .header(
                reqwest::header::ACCEPT,
                "application/json, application/geo+json",
            )
            .send()
            .await
            .with_context(|| format!("geojson_poll fetch failed for {}", url))?;

        if resp.status() == StatusCode::FOUND
            || resp.status() == StatusCode::MOVED_PERMANENTLY
            || resp.status() == StatusCode::TEMPORARY_REDIRECT
            || resp.status() == StatusCode::PERMANENT_REDIRECT
            || resp.status() == StatusCode::SEE_OTHER
        {
            bail!("geojson_poll fetch failed: redirects are not allowed");
        }
        if !resp.status().is_success() {
            bail!("geojson_poll fetch failed: HTTP {}", resp.status().as_u16());
        }

        let bytes = resp
            .bytes()
            .await
            .context("failed to read geojson_poll response body")?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("geojson_poll response too large");
        }

        serde_json::from_slice(&bytes).context("geojson_poll response is not valid JSON")
    }

    pub(crate) fn items<'a>(&self, body: &'a Value) -> anyhow::Result<&'a Vec<Value>> {
        let selected = if self.config.items_pointer.is_empty() {
            body
        } else {
            body.pointer(&self.config.items_pointer).ok_or_else(|| {
                anyhow!(
                    "geojson_poll items_pointer '{}' not found",
                    self.config.items_pointer
                )
            })?
        };

        selected.as_array().ok_or_else(|| {
            anyhow!(
                "geojson_poll items_pointer '{}' did not resolve to an array",
                self.config.items_pointer
            )
        })
    }

    fn timeout_ms(&self) -> u64 {
        self.config.timeout_ms.min(MAX_TIMEOUT_MS)
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for GeoJsonPollConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![StreamDescriptor {
            name: self.config.stream_name.clone(),
            schema: json!({
                "type": "object",
                "description": "GeoJSON/JSON feed feature"
            }),
            view_config: self.config.view_config.clone(),
        }])
    }

    async fn poll(&self, stream_name: &str, _cursor: Option<Value>) -> anyhow::Result<PollResult> {
        if stream_name != self.config.stream_name {
            bail!("geojson_poll stream '{}' not found in config", stream_name);
        }

        let body = self.fetch_feed_json().await?;
        let items = self.items(&body)?;
        let max_items = self.config.max_items.unwrap_or(DEFAULT_MAX_ITEMS);

        let mut events = Vec::new();
        for feature in items.iter().take(max_items) {
            if !self.type_allowed(feature) {
                continue;
            }

            let observed_at = match self.observed_at(feature) {
                Some(observed_at) => observed_at,
                None => continue,
            };
            let dedup_key = match self.dedup_key(feature) {
                DedupKeyResult::Keep(key) => key,
                DedupKeyResult::Skip => continue,
            };

            events.push(StreamEventInput {
                payload: feature.clone(),
                observed_at,
                dedup_key,
            });
        }

        Ok(PollResult {
            events,
            next_cursor: None,
        })
    }
}

impl GeoJsonPollConnector {
    fn type_allowed(&self, feature: &Value) -> bool {
        let Some(filter) = &self.config.type_filter else {
            return true;
        };
        let Some(value) = feature.pointer(&filter.pointer).and_then(Value::as_str) else {
            return false;
        };
        filter.allow.iter().any(|allowed| allowed == value)
    }

    fn observed_at(&self, feature: &Value) -> Option<DateTime<Utc>> {
        match self.config.observed_at_format {
            ObservedAtFormat::None => Some(Utc::now()),
            ObservedAtFormat::Rfc3339 => {
                let ptr = self.config.observed_at_pointer.as_deref()?;
                let raw = feature.pointer(ptr).and_then(Value::as_str)?;
                match DateTime::parse_from_rfc3339(raw) {
                    Ok(dt) => Some(dt.with_timezone(&Utc)),
                    Err(err) => {
                        warn!(error = %err, pointer = ptr, "geojson_poll skipped feature with invalid rfc3339 timestamp");
                        None
                    }
                }
            }
            ObservedAtFormat::EpochMs => {
                let ptr = self.config.observed_at_pointer.as_deref()?;
                let raw = feature.pointer(ptr)?;
                let millis = raw
                    .as_i64()
                    .or_else(|| raw.as_u64().and_then(|v| i64::try_from(v).ok()))?;
                match Utc.timestamp_millis_opt(millis).single() {
                    Some(dt) => Some(dt),
                    None => {
                        warn!(
                            pointer = ptr,
                            "geojson_poll skipped feature with invalid epoch_ms timestamp"
                        );
                        None
                    }
                }
            }
            ObservedAtFormat::EpochS => {
                let ptr = self.config.observed_at_pointer.as_deref()?;
                let raw = feature.pointer(ptr)?;
                let seconds = raw
                    .as_i64()
                    .or_else(|| raw.as_u64().and_then(|v| i64::try_from(v).ok()))?;
                match Utc.timestamp_opt(seconds, 0).single() {
                    Some(dt) => Some(dt),
                    None => {
                        warn!(
                            pointer = ptr,
                            "geojson_poll skipped feature with invalid epoch_s timestamp"
                        );
                        None
                    }
                }
            }
        }
    }

    fn dedup_key(&self, feature: &Value) -> DedupKeyResult {
        let Some(ptr) = self.config.dedup_pointer.as_deref() else {
            return DedupKeyResult::Keep(None);
        };
        let Some(raw) = feature.pointer(ptr).and_then(Value::as_str) else {
            warn!(
                pointer = ptr,
                "geojson_poll skipped feature with missing dedup key"
            );
            return DedupKeyResult::Skip;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_DEDUP_KEY_LEN {
            warn!(
                pointer = ptr,
                "geojson_poll skipped feature with invalid dedup key"
            );
            return DedupKeyResult::Skip;
        }
        DedupKeyResult::Keep(Some(trimmed.to_string()))
    }
}

enum DedupKeyResult {
    Keep(Option<String>),
    Skip,
}

impl GeoJsonPollConfig {
    fn validate(&self) -> anyhow::Result<()> {
        if self.kind != "geojson_poll" {
            bail!("geojson_poll config.kind must be 'geojson_poll'");
        }
        if self.stream_name.trim().is_empty() || self.stream_name.len() > 255 {
            bail!("geojson_poll stream_name must be non-empty and <= 255 chars");
        }
        url_guard::parse_and_validate_url(&self.feed_url, "feed_url")?;
        validate_json_pointer(&self.items_pointer)
            .context("geojson_poll items_pointer is not a valid JSON Pointer")?;
        if !matches!(self.observed_at_format, ObservedAtFormat::None)
            && self.observed_at_pointer.is_none()
        {
            bail!("geojson_poll observed_at_pointer is required unless observed_at_format=none");
        }
        if let Some(ptr) = &self.observed_at_pointer {
            validate_json_pointer(ptr)
                .context("geojson_poll observed_at_pointer is not a valid JSON Pointer")?;
        }
        if let Some(ptr) = &self.dedup_pointer {
            validate_json_pointer(ptr)
                .context("geojson_poll dedup_pointer is not a valid JSON Pointer")?;
        }
        if let Some(filter) = &self.type_filter {
            validate_json_pointer(&filter.pointer)
                .context("geojson_poll type_filter.pointer is not a valid JSON Pointer")?;
            if filter.allow.is_empty() || filter.allow.iter().any(|v| v.is_empty()) {
                bail!("geojson_poll type_filter.allow must contain non-empty strings");
            }
        }
        if let Some(max_items) = self.max_items {
            if !(1..=MAX_ITEMS_LIMIT).contains(&max_items) {
                bail!("geojson_poll max_items must be 1..=10000");
            }
        }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_TIMEOUT_MS {
            bail!("geojson_poll timeout_ms must be 1..=30000");
        }
        if let Some(view_config) = &self.view_config {
            validate_view_config(view_config).map_err(|err| anyhow!(err))?;
        }
        if let Some(field_types) = &self.field_types {
            for pointer in field_types.keys() {
                validate_json_pointer(pointer)
                    .context("geojson_poll field_types key is not a valid JSON Pointer")?;
            }
        }
        Ok(())
    }
}

fn default_items_pointer() -> String {
    "/features".to_string()
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[cfg(test)]
mod tests {
    use super::GeoJsonPollConnector;
    use serde_json::json;

    #[tokio::test]
    async fn skips_features_with_missing_or_empty_dedup_key() {
        let config = json!({
            "kind": "geojson_poll",
            "feed_url": "http://127.0.0.1:1/feed",
            "stream_name": "earthquakes",
            "observed_at_pointer": "/properties/time",
            "observed_at_format": "epoch_ms",
            "dedup_pointer": "/id"
        });
        let connector = GeoJsonPollConnector::from_config(&config).expect("valid config");
        let body = json!({
            "features": [
                { "id": "valid", "properties": { "time": 1779991039445_i64 } },
                { "properties": { "time": 1779991039445_i64 } },
                { "id": "", "properties": { "time": 1779991039445_i64 } }
            ]
        });

        let items = connector.items(&body).expect("items");
        let kept = items
            .iter()
            .filter(|feature| {
                matches!(
                    connector.dedup_key(feature),
                    super::DedupKeyResult::Keep(Some(_))
                )
            })
            .count();
        assert_eq!(kept, 1);
    }

    #[test]
    fn epoch_ms_timestamp_parses() {
        let config = json!({
            "kind": "geojson_poll",
            "feed_url": "http://127.0.0.1:1/feed",
            "stream_name": "earthquakes",
            "observed_at_pointer": "/properties/time",
            "observed_at_format": "epoch_ms"
        });
        let connector = GeoJsonPollConnector::from_config(&config).expect("valid config");
        let dt = connector
            .observed_at(&json!({ "properties": { "time": 1779991039445_i64 } }))
            .expect("timestamp");
        assert_eq!(dt.to_rfc3339(), "2026-05-28T17:57:19.445+00:00");
    }

    #[test]
    fn geojson_poll_field_types_validate_json_pointers() {
        let valid = json!({
            "kind": "geojson_poll",
            "feed_url": "http://127.0.0.1:1/feed",
            "stream_name": "earthquakes",
            "observed_at_format": "none",
            "field_types": {
                "/properties/mag": "number",
                "/properties/code": "string",
                "/properties/reviewed": "boolean"
            }
        });
        GeoJsonPollConnector::from_config(&valid).expect("valid field_types");

        let invalid = json!({
            "kind": "geojson_poll",
            "feed_url": "http://127.0.0.1:1/feed",
            "stream_name": "earthquakes",
            "observed_at_format": "none",
            "field_types": {
                "properties/mag": "number"
            }
        });
        assert!(GeoJsonPollConnector::from_config(&invalid).is_err());
    }
}
