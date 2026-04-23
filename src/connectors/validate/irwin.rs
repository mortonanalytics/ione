use serde_json::{json, Value};

use super::{short_client, ValidateErr, ValidateOk, ValidateResult, Validator};

pub struct IrwinValidator;

#[async_trait::async_trait]
impl Validator for IrwinValidator {
    async fn validate(config: &Value) -> ValidateResult {
        let endpoint = config
            .get("endpoint")
            .or_else(|| config.get("base_url"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ValidateErr::new("validation_failed", "endpoint is required.")
                    .with_field("endpoint")
            })?;

        let parsed = reqwest::Url::parse(endpoint).map_err(|_| {
            ValidateErr::new("validation_failed", "endpoint is not a valid URL.")
                .with_field("endpoint")
        })?;

        let resp = short_client(5).head(endpoint).send().await.map_err(|e| {
            ValidateErr::new("network_timeout", &format!("Couldn't reach IRWIN: {e}"))
                .with_hint("Check your network or firewall, then click Test again.")
        })?;

        let status = resp.status();
        if status.is_server_error() {
            return Err(ValidateErr::new(
                "irwin_upstream_error",
                &format!("IRWIN returned {status}."),
            )
            .with_hint("Try again in a minute."));
        }

        Ok(ValidateOk {
            ok: true,
            sample: json!({ "endpointHost": parsed.host_str().unwrap_or_default() }),
        })
    }
}

pub async fn validate(config: &Value) -> ValidateResult {
    IrwinValidator::validate(config).await
}
