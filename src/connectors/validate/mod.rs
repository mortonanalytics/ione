use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub mod firms;
pub mod irwin;
pub mod nws;
pub mod s3;
pub mod slack;

#[async_trait::async_trait]
pub trait Validator {
    async fn validate(config: &Value) -> ValidateResult;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateOk {
    pub sample: Value,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ValidateErr {
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl ValidateErr {
    pub fn new(error: &str, message: &str) -> Self {
        Self {
            error: error.into(),
            message: message.into(),
            hint: None,
            field: None,
        }
    }

    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_field(mut self, field: &str) -> Self {
        self.field = Some(field.into());
        self
    }
}

pub type ValidateResult = Result<ValidateOk, ValidateErr>;

/// Dispatch a connector config to its provider-specific validator.
///
/// `name` is the connector name chosen by the user (e.g. "nws", "firms", "my-bucket").
/// For `rust_native` connectors, the name is used to pick the provider because IONe's
/// existing contract is that rust_native + name="nws" -> NWS; name="firms" -> FIRMS; etc.
pub async fn dispatch(kind: &str, name: &str, config: &Value) -> ValidateResult {
    match kind {
        "rust_native" => match rust_native_provider(name, config).as_deref() {
            Some("nws") => nws::validate(config).await,
            Some("firms") => firms::validate(config).await,
            Some("s3" | "fs_s3" | "s3_fs") => s3::validate(config).await,
            Some("slack") => slack::validate(config).await,
            Some("irwin") => irwin::validate(config).await,
            other => Err(ValidateErr::new(
                "unknown_rust_native_provider",
                &format!(
                    "No rust_native provider named '{}'. Known: nws, firms, s3, slack, irwin.",
                    other.unwrap_or(name)
                ),
            )
            .with_hint("Use Custom JSON if this is an unusual provider.")),
        },
        "openapi" => Err(ValidateErr::new(
            "openapi_not_implemented",
            "OpenAPI connectors are not implemented yet.",
        )),
        "mcp" => Err(ValidateErr::new(
            "mcp_validate_unsupported",
            "MCP peers are configured via the Federation wizard, not the connector form.",
        )),
        other => Err(ValidateErr::new(
            "unknown_connector_kind",
            &format!("Unknown connector kind '{other}'."),
        )),
    }
}

fn rust_native_provider(name: &str, config: &Value) -> Option<String> {
    let hint = config.get("kind").and_then(Value::as_str).unwrap_or("");
    let hint = hint.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let candidate = if hint.is_empty() {
        name.as_str()
    } else {
        hint.as_str()
    };

    if candidate == "nws" || name.starts_with("nws") {
        Some("nws".to_string())
    } else if candidate == "firms" || name.starts_with("firms") {
        Some("firms".to_string())
    } else if matches!(candidate, "s3" | "fs_s3" | "s3_fs" | "fs") || name.starts_with("s3") {
        Some("s3".to_string())
    } else if candidate == "slack" || name.starts_with("slack") {
        Some("slack".to_string())
    } else if candidate == "irwin" || name.starts_with("irwin") {
        Some("irwin".to_string())
    } else {
        None
    }
}

pub(crate) fn short_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(std::env::var("IONE_HTTP_UA").unwrap_or_else(|_| "IONe/0.1".into()))
        .build()
        .expect("reqwest client")
}
