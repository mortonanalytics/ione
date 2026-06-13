use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// A service-account token row. `token_hash` is the SHA-256 hex of the plaintext
/// and is never serialized; the plaintext itself is shown once at issuance and
/// never stored.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceAccountToken {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub permissions: Value,
    pub provisionable_max_coc: i32,
    pub created_by: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ServiceAccountToken {
    /// The token's permission strings as a `Vec<String>` (non-string entries
    /// dropped). Used to populate the synthetic `AuthContext`.
    pub fn permission_list(&self) -> Vec<String> {
        match &self.permissions {
            Value::Array(items) => items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        }
    }
}
