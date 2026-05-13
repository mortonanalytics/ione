use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustIssuer {
    pub id: Uuid,
    pub org_id: Uuid,
    pub issuer_url: String,
    pub audience: String,
    pub jwks_uri: String,
    pub claim_mapping: Value,
    pub idp_type: String,
    pub max_coc_level: i32,
    pub client_id: Option<String>,
    #[serde(skip_serializing)]
    pub client_secret_ciphertext: Option<Vec<u8>>,
    pub display_name: Option<String>,
}
