use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::ActorKind;

pub mod outcome {
    pub const ALLOW: &str = "allow";
    pub const DENY: &str = "deny";
    pub const PENDING: &str = "pending";
    pub const ERROR: &str = "error";

    pub fn is_valid(value: &str) -> bool {
        matches!(value, ALLOW | DENY | PENDING | ERROR)
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionEvent {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub peer_name: String,
    pub tool_name: String,
    pub caller_kind: ActorKind,
    pub caller_user_id: Option<Uuid>,
    pub caller_peer_id: Option<Uuid>,
    pub caller_token_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub sequence_number: Option<i64>,
    pub outcome: String,
    pub latency_ms: Option<i32>,
    pub detail: serde_json::Value,
    pub recorded_at: DateTime<Utc>,
}
