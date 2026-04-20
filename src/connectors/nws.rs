use anyhow::Context;
use serde_json::json;

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

const DEFAULT_UA: &str = "IONe/0.1 (github.com/morton-analytics/ione; contact:morton@myma.us)";

pub struct NwsConnector {
    pub lat: f64,
    pub lon: f64,
    pub http: reqwest::Client,
    pub ua: String,
}

impl NwsConnector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let lat = config["lat"]
            .as_f64()
            .context("NWS connector config missing 'lat'")?;
        let lon = config["lon"]
            .as_f64()
            .context("NWS connector config missing 'lon'")?;
        let ua = std::env::var("IONE_HTTP_UA").unwrap_or_else(|_| DEFAULT_UA.to_string());
        Ok(Self {
            lat,
            lon,
            http: reqwest::Client::new(),
            ua,
        })
    }
}

#[async_trait::async_trait]
impl ConnectorImpl for NwsConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![StreamDescriptor {
            name: "observations".to_string(),
            schema: json!({
                "type": "object",
                "description": "NWS latest observations"
            }),
        }])
    }

    async fn poll(
        &self,
        stream_name: &str,
        _cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        if stream_name != "observations" {
            anyhow::bail!(
                "NWS connector only supports stream 'observations', got '{}'",
                stream_name
            );
        }

        // Step 1: resolve the forecast grid point to get observation stations URL
        let points_url = format!(
            "https://api.weather.gov/points/{:.4},{:.4}",
            self.lat, self.lon
        );

        let points_resp = self
            .http
            .get(&points_url)
            .header("User-Agent", &self.ua)
            .header("Accept", "application/geo+json")
            .send()
            .await
            .context("NWS /points request failed")?;

        if !points_resp.status().is_success() {
            anyhow::bail!(
                "NWS /points returned status {} for lat={} lon={}",
                points_resp.status(),
                self.lat,
                self.lon
            );
        }

        let points_body: serde_json::Value = points_resp
            .json()
            .await
            .context("failed to parse NWS /points response as JSON")?;

        let stations_url = points_body["properties"]["observationStations"]
            .as_str()
            .context("NWS /points response missing 'properties.observationStations'")?
            .to_string();

        // Step 2: get the list of observation stations
        let stations_resp = self
            .http
            .get(&stations_url)
            .header("User-Agent", &self.ua)
            .header("Accept", "application/geo+json")
            .send()
            .await
            .context("NWS observationStations request failed")?;

        if !stations_resp.status().is_success() {
            anyhow::bail!(
                "NWS observationStations returned status {}",
                stations_resp.status()
            );
        }

        let stations_body: serde_json::Value = stations_resp
            .json()
            .await
            .context("failed to parse NWS observationStations response as JSON")?;

        // Take the first station from the FeatureCollection
        let station_id = stations_body["features"]
            .as_array()
            .and_then(|f| f.first())
            .and_then(|f| f["properties"]["stationIdentifier"].as_str())
            .context("NWS observationStations: could not find first station identifier")?
            .to_string();

        // Step 3: get the latest observation for that station
        let obs_url = format!(
            "https://api.weather.gov/stations/{}/observations/latest",
            station_id
        );

        let obs_resp = self
            .http
            .get(&obs_url)
            .header("User-Agent", &self.ua)
            .header("Accept", "application/geo+json")
            .send()
            .await
            .context("NWS observations/latest request failed")?;

        if !obs_resp.status().is_success() {
            anyhow::bail!(
                "NWS observations/latest returned status {}",
                obs_resp.status()
            );
        }

        let obs_body: serde_json::Value = obs_resp
            .json()
            .await
            .context("failed to parse NWS observations/latest response as JSON")?;

        let properties = obs_body["properties"].clone();

        let timestamp_str = properties["timestamp"]
            .as_str()
            .context("NWS observation missing 'properties.timestamp'")?;

        let observed_at = chrono::DateTime::parse_from_rfc3339(timestamp_str)
            .context("failed to parse NWS observation timestamp as RFC3339")?
            .with_timezone(&chrono::Utc);

        Ok(PollResult {
            events: vec![StreamEventInput {
                payload: properties,
                observed_at,
            }],
            next_cursor: None,
        })
    }
}
