use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetIdentity {
    pub deployment_id: Uuid,
    pub tenant_id: Uuid,
    pub workspace_id: Uuid,
    pub actor_id: String,
    pub service_account_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetDelegation {
    pub grant_id: Uuid,
    pub owner: DatasetIdentity,
    pub origin: DatasetIdentity,
    pub dataset_id: Uuid,
    pub version_id: Uuid,
    pub columns: Vec<String>,
    pub arrow_schema_hash: String,
    pub content_digest: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDelegation {
    pub origin: DatasetIdentity,
    #[serde(rename = "ttlSeconds")]
    pub ttl_seconds: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRead {
    pub columns: Vec<String>,
    pub limit: Option<u64>,
    pub max_rows: u64,
    pub max_bytes: u64,
}
