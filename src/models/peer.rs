use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "peer_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PeerStatus {
    Active,
    Paused,
    Error,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: Uuid,
    pub name: String,
    pub mcp_url: String,
    pub issuer_id: Uuid,
    pub sharing_policy: serde_json::Value,
    pub status: PeerStatus,
    pub created_at: DateTime<Utc>,
}
