use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OauthClient {
    pub id: Uuid,
    pub client_id: String,
    pub client_metadata: Value,
    pub registered_by_user_id: Option<Uuid>,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
pub struct OauthAuthCode {
    pub code: String,
    pub client_id: String,
    pub user_id: Uuid,
    pub redirect_uri: String,
    pub scope: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
pub struct OauthAccessToken {
    pub token_hash: String,
    pub client_id: String,
    pub user_id: Uuid,
    pub scope: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
pub struct OauthRefreshToken {
    pub token_hash: String,
    pub client_id: String,
    pub user_id: Uuid,
    pub scope: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// CIMD client metadata — used to parse the JSONB and render safe summaries.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClientMetadata {
    pub client_name: Option<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    pub scope: Option<String>,
    #[serde(default)]
    pub grant_types: Vec<String>,
    #[serde(default)]
    pub response_types: Vec<String>,
}
