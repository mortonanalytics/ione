/// NASA FIRMS (Fire Information for Resource Management System) connector.
///
/// Polls the FIRMS area API for VIIRS_SNPP_NRT hotspot detections and emits
/// one `StreamEventInput` per CSV row.
///
/// Config shape:
/// ```json
/// {
///   "map_key": "YOUR_KEY",
///   "area": "MONTANA",            // named area alias
///   "days": 1,                    // 1..10
///   // OR instead of "area", use bbox:
///   "north": 49.0, "south": 44.5, "east": -104.0, "west": -116.0
/// }
/// ```
///
/// Demo / offline mode: if `map_key` is absent, empty, or starts with `DEMO_`,
/// the connector reads `infra/fixtures/firms_sample.csv` instead of calling the
/// network.  This lets the demo run without a real FIRMS API key.
///
/// Real API: https://firms.modaps.eosdis.nasa.gov/api/area/csv/{map_key}/VIIRS_SNPP_NRT/{area}/{days}
use anyhow::Context;
use serde_json::json;

use crate::models::ConnectorKind;

use super::{ConnectorImpl, PollResult, StreamDescriptor, StreamEventInput};

pub struct FirmsConnector {
    pub map_key: String,
    pub area: String,
    pub days: u8,
    pub http: reqwest::Client,
    /// If true, reads from fixture file instead of network.
    pub demo_mode: bool,
    /// Path to the fixture CSV for demo/offline mode.
    pub fixture_path: String,
}

impl FirmsConnector {
    pub fn from_config(config: &serde_json::Value) -> anyhow::Result<Self> {
        let map_key = config["map_key"].as_str().unwrap_or("").to_string();
        let demo_mode = map_key.is_empty() || map_key.starts_with("DEMO_");

        let area = build_area_param(config);

        let days = config["days"].as_u64().unwrap_or(1).clamp(1, 10) as u8;

        let fixture_path = config["fixture_path"]
            .as_str()
            .unwrap_or("infra/fixtures/firms_sample.csv")
            .to_string();

        Ok(Self {
            map_key,
            area,
            days,
            http: reqwest::Client::new(),
            demo_mode,
            fixture_path,
        })
    }

    async fn fetch_csv(&self) -> anyhow::Result<String> {
        if self.demo_mode {
            return read_fixture_csv(&self.fixture_path);
        }
        let url = format!(
            "https://firms.modaps.eosdis.nasa.gov/api/area/csv/{}/VIIRS_SNPP_NRT/{}/{}",
            self.map_key, self.area, self.days
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("FIRMS HTTP request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("FIRMS API returned status {}", resp.status());
        }
        resp.text().await.context("FIRMS response read failed")
    }
}

fn build_area_param(config: &serde_json::Value) -> String {
    // Named area alias takes priority.
    if let Some(area) = config["area"].as_str() {
        return area.to_string();
    }
    // Bbox fallback: north,south,east,west.
    if let (Some(n), Some(s), Some(e), Some(w)) = (
        config["north"].as_f64(),
        config["south"].as_f64(),
        config["east"].as_f64(),
        config["west"].as_f64(),
    ) {
        return format!("{},{},{},{}", n, s, e, w);
    }
    // Default: CONUS.
    "CONUS".to_string()
}

fn read_fixture_csv(path: &str) -> anyhow::Result<String> {
    std::fs::read_to_string(path)
        .or_else(|_| {
            // Try relative to the manifest dir (for tests).
            let manifest = env!("CARGO_MANIFEST_DIR");
            let full = format!("{}/{}", manifest, path);
            std::fs::read_to_string(&full)
        })
        .context("failed to read FIRMS fixture CSV")
}

fn parse_csv_events(csv: &str) -> anyhow::Result<Vec<StreamEventInput>> {
    let mut reader = csv::Reader::from_reader(csv.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .context("FIRMS CSV missing header row")?
        .iter()
        .map(str::to_string)
        .collect();

    let mut events = Vec::new();
    for result in reader.records() {
        let record = result.context("FIRMS CSV row parse failed")?;
        let row = build_row_payload(&headers, &record);
        let observed_at = parse_observed_at(&row)?;
        events.push(StreamEventInput {
            payload: row,
            observed_at,
            dedup_key: None,
        });
    }
    Ok(events)
}

fn build_row_payload(headers: &[String], record: &csv::StringRecord) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (i, key) in headers.iter().enumerate() {
        let val = record.get(i).unwrap_or("").trim();
        // Coerce numeric columns.
        if let Ok(f) = val.parse::<f64>() {
            map.insert(key.clone(), json!(f));
        } else {
            map.insert(key.clone(), json!(val));
        }
    }
    serde_json::Value::Object(map)
}

fn parse_observed_at(row: &serde_json::Value) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    let date = row["acq_date"]
        .as_str()
        .context("FIRMS row missing 'acq_date'")?;

    // acq_time is a 4-digit HHMM string stored as a float (e.g. 1425.0 → "1425").
    let time_raw = if let Some(f) = row["acq_time"].as_f64() {
        format!("{:04}", f as u64)
    } else {
        row["acq_time"].as_str().unwrap_or("0000").to_string()
    };

    let dt_str = format!("{} {:04} UTC", date, time_raw.parse::<u64>().unwrap_or(0));
    chrono::NaiveDateTime::parse_from_str(&dt_str, "%Y-%m-%d %H%M UTC")
        .map(|ndt| ndt.and_utc())
        .context("failed to parse FIRMS acq_date+acq_time")
}

#[async_trait::async_trait]
impl ConnectorImpl for FirmsConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::RustNative
    }

    async fn default_streams(&self) -> anyhow::Result<Vec<StreamDescriptor>> {
        Ok(vec![StreamDescriptor {
            name: "hotspots".to_string(),
            schema: json!({
                "type": "object",
                "description": "NASA FIRMS VIIRS_SNPP_NRT hotspot detections"
            }),
            // FIRMS CSV columns are lowercase (latitude/longitude/bright_ti4/frp).
            view_config: Some(json!({
                "lon_pointer": "/longitude",
                "lat_pointer": "/latitude",
                "property_fields": [
                    { "pointer": "/bright_ti4", "name": "bright_ti4" },
                    { "pointer": "/frp", "name": "frp" }
                ],
                "attribution": "NASA FIRMS (VIIRS_SNPP_NRT)",
                "style": {
                    "size_field": "frp",
                    "size_domain": [0.0, 100.0],
                    "size_range": [4.0, 20.0],
                    "color_field": "bright_ti4",
                    "color_domain": [300.0, 350.0, 400.0],
                    "color_range": ["#f5d76e", "#d9534f", "#3a0ca3"]
                }
            })),
        }])
    }

    async fn poll(
        &self,
        stream_name: &str,
        _cursor: Option<serde_json::Value>,
    ) -> anyhow::Result<PollResult> {
        if stream_name != "hotspots" {
            anyhow::bail!(
                "FIRMS connector only supports stream 'hotspots', got '{}'",
                stream_name
            );
        }
        let csv = self.fetch_csv().await?;
        let events = parse_csv_events(&csv)?;
        Ok(PollResult {
            events,
            next_cursor: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn view_config_pointers_resolve_against_fixture() {
        let conn = FirmsConnector::from_config(&json!({})).expect("demo connector");
        let descriptors = conn.default_streams().await.expect("default_streams");
        let vc = descriptors[0]
            .view_config
            .as_ref()
            .expect("FIRMS stream must declare view_config");

        let events = conn.poll("hotspots", None).await.expect("poll fixture");
        let payload = &events.events[0].payload;

        for key in ["lon_pointer", "lat_pointer"] {
            let pointer = vc[key].as_str().expect("pointer is a string");
            let resolved = payload.pointer(pointer).and_then(serde_json::Value::as_f64);
            assert!(
                resolved.is_some(),
                "{key} ({pointer}) must resolve to a number in a real FIRMS payload"
            );
        }
    }
}
