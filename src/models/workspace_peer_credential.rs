use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Metadata for a pre-broker static credential scoped to one (workspace, peer).
///
/// `credential_ciphertext` is deliberately absent: every read surface returns
/// this type, so neither the ciphertext nor the plaintext can be echoed by
/// construction. The plaintext is returned exactly once, by the create/rotate
/// handler, from the value it just stored.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePeerCredential {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub rotated_at: Option<DateTime<Utc>>,
}
