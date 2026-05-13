use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerCredential {
    pub id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub provider: String,
    pub label: String,
    pub scopes: Vec<String>,
    #[serde(skip_serializing)]
    pub access_token_ciphertext: Option<Vec<u8>>,
    #[serde(skip_serializing)]
    pub refresh_token_ciphertext: Option<Vec<u8>>,
    pub token_expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing)]
    pub state_token: Option<String>,
    #[serde(skip_serializing)]
    pub code_verifier: Option<String>,
    pub state_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
