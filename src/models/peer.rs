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
    pub org_id: Uuid,
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
    #[serde(skip_serializing)]
    pub refresh_token_ciphertext: Option<Vec<u8>>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub tool_allowlist: serde_json::Value,
    pub tool_prefix: Option<String>,
    pub session_status: String,
    pub last_connected_at: Option<DateTime<Utc>>,
    pub last_session_error: Option<String>,
    pub last_manifest_jsonb: Option<serde_json::Value>,
    /// Workspace this peer handle was resolved for. Not a `peers` column: it
    /// carries the outbound auth scope so `services::peer_tokens` can present
    /// the pre-broker per-(workspace, peer) static credential. `None` on
    /// peer-global paths (registration-time manifest fetch, the long-lived SSE
    /// session), which have no workspace and stay on the OAuth/env path.
    #[sqlx(default)]
    #[serde(skip)]
    pub workspace_scope: Option<Uuid>,
}

impl Peer {
    /// Scope this peer handle to `workspace_id` for outbound auth resolution.
    pub fn scoped_to(mut self, workspace_id: Uuid) -> Self {
        self.workspace_scope = Some(workspace_id);
        self
    }
}
