use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Membership {
    pub id: Uuid,
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    pub role_id: Uuid,
    pub federated_claim_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}
