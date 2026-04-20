use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "signal_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    Rule,
    ConnectorEvent,
    Generator,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "severity", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Routine,
    Flagged,
    Command,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub source: SignalSource,
    pub title: String,
    pub body: String,
    pub evidence: serde_json::Value,
    pub severity: Severity,
    pub generator_model: Option<String>,
    pub created_at: DateTime<Utc>,
}
