use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagStatus {
    Ok,
    StreamNotFound,
    NoEvents,
    ParseError,
    TypeMismatch,
    RulesUnparseable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkipReason {
    pub code: String,
    pub detail: String,
    pub count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDiagnostic {
    pub rule_index: i64,
    pub rule_title: String,
    pub stream: String,
    pub status: DiagStatus,
    pub events_evaluated: i64,
    pub match_count: i64,
    pub skip_reasons: Vec<SkipReason>,
}

#[derive(Clone, Debug)]
pub struct RuleEvalReport {
    pub inserted: usize,
    pub diagnostics: Vec<RuleDiagnostic>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct RuleDiagnosticSnapshot {
    pub workspace_id: Uuid,
    pub evaluated_at: DateTime<Utc>,
    pub items: serde_json::Value,
}
