use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::Severity;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "critic_verdict", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CriticVerdict {
    Survive,
    Reject,
    Defer,
}

impl CriticVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            CriticVerdict::Survive => "survive",
            CriticVerdict::Reject => "reject",
            CriticVerdict::Defer => "defer",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Survivor {
    pub id: Uuid,
    pub signal_id: Uuid,
    pub critic_model: String,
    pub verdict: CriticVerdict,
    pub rationale: String,
    pub confidence: f32,
    pub chain_of_reasoning: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Extended projection that joins survivor with its parent signal fields.
/// Used by the list endpoint so the UI can display signal context without
/// a second request.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurvivorRow {
    pub id: Uuid,
    pub signal_id: Uuid,
    pub critic_model: String,
    pub verdict: CriticVerdict,
    pub rationale: String,
    pub confidence: f32,
    pub chain_of_reasoning: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub signal_title: String,
    pub signal_body: String,
    pub signal_severity: Severity,
}
