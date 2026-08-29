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
    /// True once `POST /api/v1/peers/:id/authorize` wrote `tool_allowlist`.
    /// Disambiguates an authorized-but-empty allowlist (deny everything) from a
    /// row that still carries the column default. See `tool_is_allowlisted`.
    ///
    /// Deliberately not `#[sqlx(default)]`: a projection that forgets this
    /// column must fail as `ColumnNotFound` at the query, not hydrate `false`
    /// and disable the allowlist gate three layers from the predicate that
    /// reads it. `#[serde(default)]` stays — it governs JSON bodies, not row
    /// hydration.
    #[serde(default)]
    pub tool_allowlist_configured: bool,
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
    /// The single source every `peers` projection renders from. Hydrating
    /// `Peer` from a hand-written list is how `tool_allowlist_configured` went
    /// missing in one of three places and quietly turned the allowlist gate off
    /// (issue #26). Add a column to `Peer`, add it here, and every projection
    /// has it.
    pub const COLUMNS: &'static str =
        "id, org_id, name, mcp_url, issuer_id, sharing_policy, status, created_at, \
         oauth_client_id, access_token_hash, refresh_token_hash, access_token_ciphertext, \
         refresh_token_ciphertext, token_expires_at, tool_allowlist, tool_allowlist_configured, \
         tool_prefix, session_status, last_connected_at, last_session_error, last_manifest_jsonb";

    /// `peers` columns deliberately outside [`Peer::COLUMNS`]. The secret
    /// ciphertext is read only by `PeerRepo::get_with_webhook_secret`, which
    /// asks for it by name; keeping it out of the shared list means no ordinary
    /// peer read carries it.
    pub const NON_PROJECTED_COLUMNS: &'static [&'static str] = &["webhook_secret_ciphertext"];

    /// [`Peer::COLUMNS`] qualified with a table alias, for projections that
    /// join `peers` to another table.
    pub fn columns_aliased(alias: &str) -> String {
        Self::COLUMNS
            .split(',')
            .map(|column| format!("{alias}.{}", column.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Scope this peer handle to `workspace_id` for outbound auth resolution.
    pub fn scoped_to(mut self, workspace_id: Uuid) -> Self {
        self.workspace_scope = Some(workspace_id);
        self
    }
}
