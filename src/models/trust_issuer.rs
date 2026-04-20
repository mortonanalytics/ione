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
}
