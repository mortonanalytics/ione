use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Metadata for a brokered delegated token scoped to one (workspace, peer).
///
/// Neither ciphertext column is present: every read surface returns this type,
/// so no API response can echo delegated token material by construction. The
/// plaintext is never returned by any endpoint — only `services::peer_tokens`
/// decrypts it, on the outbound MCP path.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePeerDelegation {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub granted_by: Option<Uuid>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub refreshed_at: Option<DateTime<Utc>>,
}
