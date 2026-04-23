use serde_json::{json, Value};

use super::{short_client, ValidateErr, ValidateOk, ValidateResult, Validator};

pub struct FirmsValidator;

#[async_trait::async_trait]
impl Validator for FirmsValidator {
    async fn validate(config: &Value) -> ValidateResult {
        let map_key = config
            .get("mapKey")
            .or_else(|| config.get("map_key"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ValidateErr::new("validation_failed", "MAP_KEY is required.").with_field("mapKey")
            })?;

        let country = config
            .get("country")
            .or_else(|| config.get("area"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("USA");

        let url = format!(
            "https://firms.modaps.eosdis.nasa.gov/api/country/csv/{map_key}/MODIS_NRT/{country}/1"
        );
        let resp = short_client(6).get(&url).send().await.map_err(|e| {
            ValidateErr::new("network_timeout", &format!("Couldn't reach FIRMS: {e}"))
                .with_hint("Check your network or firewall, then click Test again.")
        })?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(
                ValidateErr::new("firms_auth_failed", "FIRMS rejected the MAP_KEY.")
                    .with_hint("Check your MAP_KEY at firms.modaps.eosdis.nasa.gov/api.")
                    .with_field("mapKey"),
            );
        }
        if !status.is_success() {
            return Err(ValidateErr::new(
                "firms_upstream_error",
                &format!("FIRMS returned {status}."),
            )
            .with_hint("Try again in a minute."));
        }

        let body = resp.text().await.unwrap_or_default();
        let detection_count = body.lines().count().saturating_sub(1);

        Ok(ValidateOk {
            ok: true,
            sample: json!({ "detectionCount": detection_count }),
        })
    }
}

pub async fn validate(config: &Value) -> ValidateResult {
    FirmsValidator::validate(config).await
}
