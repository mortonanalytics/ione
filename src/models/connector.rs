use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    pub config: serde_json::Value,
    pub status: ConnectorStatus,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}
