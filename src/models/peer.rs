use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "peer_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    PendingOauth,
    PendingAllowlist,
    Active,
    Revoked,
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
    pub oauth_client_id: Option<String>,
    #[serde(skip_serializing)]
    pub access_token_hash: Option<String>,
    #[serde(skip_serializing)]
    pub refresh_token_hash: Option<String>,
    #[serde(skip_serializing)]
    pub access_token_ciphertext: Option<Vec<u8>>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub tool_allowlist: serde_json::Value,
}
