use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Serializer};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "connector_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    Mcp,
    Openapi,
    RustNative,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "connector_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ConnectorStatus {
    Active,
    Paused,
    Error,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connector {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: ConnectorKind,
    pub name: String,
    #[serde(serialize_with = "redact_connector_config")]
    pub config: serde_json::Value,
    pub status: ConnectorStatus,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn redact_connector_config<S>(config: &serde_json::Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut redacted = config.clone();
    redact_value(&mut redacted);
    redacted.serialize(serializer)
}

fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_secret_key(key) {
                    *value = serde_json::Value::String("[redacted]".to_string());
                } else {
                    redact_value(value);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("api_key")
}
