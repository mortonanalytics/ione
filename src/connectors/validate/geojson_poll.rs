use serde_json::{json, Value};

use crate::connectors::geojson_poll::GeoJsonPollConnector;

use super::{ValidateErr, ValidateOk, ValidateResult};

pub async fn validate(config: &Value) -> ValidateResult {
    let connector = GeoJsonPollConnector::from_config(config).map_err(|err| {
        ValidateErr::new("geojson_poll_config_invalid", &err.to_string())
            .with_hint("Check feed_url, JSON pointers, timestamp format, and view_config.")
    })?;

    let body = connector.fetch_feed_json().await.map_err(|err| {
        ValidateErr::new("geojson_poll_fetch_failed", &err.to_string())
            .with_hint("The feed must be reachable, return JSON, and avoid redirects.")
    })?;
    let feature_count = connector.items(&body).map_err(|err| {
        ValidateErr::new("geojson_poll_items_invalid", &err.to_string()).with_field("items_pointer")
    })?;

    Ok(ValidateOk {
        sample: json!({ "featureCount": feature_count.len() }),
    })
}
