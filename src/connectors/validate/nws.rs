use serde_json::{json, Value};

use super::{short_client, ValidateErr, ValidateOk, ValidateResult, Validator};

pub struct NwsValidator;

#[async_trait::async_trait]
impl Validator for NwsValidator {
    async fn validate(config: &Value) -> ValidateResult {
        let lat = config.get("lat").and_then(Value::as_f64).ok_or_else(|| {
            ValidateErr::new("validation_failed", "lat is required and must be a number.")
                .with_field("lat")
        })?;
        let lon = config.get("lon").and_then(Value::as_f64).ok_or_else(|| {
            ValidateErr::new("validation_failed", "lon is required and must be a number.")
                .with_field("lon")
        })?;

        if !(-90.0..=90.0).contains(&lat) {
            return Err(ValidateErr::new(
                "nws_out_of_range",
                "Latitude must be between -90 and 90.",
            )
            .with_hint("Enter a latitude in decimal degrees, for example 39.7392.")
            .with_field("lat"));
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(ValidateErr::new(
                "nws_out_of_range",
                "Longitude must be between -180 and 180.",
            )
            .with_hint("Enter a longitude in decimal degrees, for example -104.9903.")
            .with_field("lon"));
        }

        if std::env::var("IONE_SKIP_LIVE").as_deref() == Ok("1") {
            return Ok(ValidateOk {
                sample: json!({ "mode": "syntaxOnly" }),
            });
        }

        let url = format!("https://api.weather.gov/alerts/active?point={lat},{lon}");
        let resp = short_client(5).get(&url).send().await.map_err(|e| {
            ValidateErr::new(
                "network_timeout",
                &format!("Couldn't reach api.weather.gov: {e}"),
            )
            .with_hint("Check your network or firewall, then click Test again.")
        })?;

        if !resp.status().is_success() {
            return Err(ValidateErr::new(
                "nws_upstream_error",
                &format!("NWS returned {}.", resp.status()),
            )
            .with_hint("Try again in a minute. NWS occasionally returns transient errors."));
        }

        let body: Value = resp.json().await.map_err(|e| {
            ValidateErr::new(
                "nws_parse_error",
                &format!("Couldn't parse NWS response: {e}"),
            )
            .with_hint("Got a response but couldn't understand it. Try Custom JSON if this is an unusual provider.")
        })?;
        let alert_count = body
            .get("features")
            .and_then(Value::as_array)
            .map(std::vec::Vec::len)
            .unwrap_or(0);

        Ok(ValidateOk {
            sample: json!({ "alertCount": alert_count }),
        })
    }
}

pub async fn validate(config: &Value) -> ValidateResult {
    NwsValidator::validate(config).await
}
