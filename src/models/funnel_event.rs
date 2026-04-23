use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FunnelEvent {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub session_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub event_kind: String,
    pub detail: Option<Value>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct FunnelEventInput {
    pub user_id: Option<Uuid>,
    pub session_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub event_kind: String,
    pub detail: Option<Value>,
}
