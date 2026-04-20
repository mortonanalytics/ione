use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "routing_target", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum RoutingTarget {
    Feed,
    Notification,
    Draft,
    Peer,
}

impl RoutingTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutingTarget::Feed => "feed",
            RoutingTarget::Notification => "notification",
            RoutingTarget::Draft => "draft",
            RoutingTarget::Peer => "peer",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingDecision {
    pub id: Uuid,
    pub survivor_id: Uuid,
    pub target_kind: RoutingTarget,
    pub target_ref: serde_json::Value,
    pub classifier_model: String,
    pub rationale: String,
    pub created_at: DateTime<Utc>,
}
