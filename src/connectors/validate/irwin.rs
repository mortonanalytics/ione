use serde_json::{json, Value};

use super::{ValidateErr, ValidateOk, ValidateResult, Validator};

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

        let status =
            crate::util::safe_http::public_head(endpoint, std::time::Duration::from_secs(5))
                .await
                .map_err(|_| {
                    ValidateErr::new("validation_failed", "endpoint must be a public URL.")
                        .with_field("endpoint")
                })?;
        if status.is_server_error() {
            return Err(ValidateErr::new(
                "irwin_upstream_error",
                &format!("IRWIN returned {status}."),
            )
            .with_hint("Try again in a minute."));
        }

        Ok(ValidateOk {
            sample: json!({ "endpointHost": parsed.host_str().unwrap_or_default() }),
        })
    }
}

pub async fn validate(config: &Value) -> ValidateResult {
    IrwinValidator::validate(config).await
}
