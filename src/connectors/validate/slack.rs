use serde_json::{json, Value};

use super::{short_client, ValidateErr, ValidateOk, ValidateResult, Validator};

pub struct SlackValidator;

#[async_trait::async_trait]
impl Validator for SlackValidator {
    async fn validate(config: &Value) -> ValidateResult {
        let webhook = config
            .get("webhookUrl")
            .or_else(|| config.get("webhook_url"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ValidateErr::new("validation_failed", "webhookUrl is required.")
                    .with_field("webhookUrl")
            })?;

        let parsed = reqwest::Url::parse(webhook).map_err(|_| {
            ValidateErr::new("validation_failed", "webhookUrl is not a valid URL.")
                .with_field("webhookUrl")
        })?;
        if parsed.host_str() != Some("hooks.slack.com") {
            return Err(ValidateErr::new(
                "slack_host_unexpected",
                "Slack webhooks are served from hooks.slack.com.",
            )
            .with_hint("Copy the webhook URL directly from Slack's incoming-webhook settings.")
            .with_field("webhookUrl"));
        }

        let resp = short_client(5).head(webhook).send().await.map_err(|e| {
            ValidateErr::new(
                "network_timeout",
                &format!("Couldn't reach hooks.slack.com: {e}"),
            )
            .with_hint("Check your network or firewall, then click Test again.")
        })?;
        if resp.status().as_u16() >= 500 {
            return Err(ValidateErr::new(
                "slack_upstream_error",
                &format!("Slack returned {}.", resp.status()),
            ));
        }

        Ok(ValidateOk {
            sample: json!({ "webhookHost": "hooks.slack.com" }),
        })
    }
}

pub async fn validate(config: &Value) -> ValidateResult {
    SlackValidator::validate(config).await
}
