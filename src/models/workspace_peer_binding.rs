use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "binding_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    Active,
    Pending,
    Conflict,
    Inactive,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePeerBinding {
    pub id: Uuid,
    pub org_id: Uuid,
    pub workspace_id: Uuid,
    pub peer_id: Uuid,
    pub foreign_tenant_id: String,
    pub foreign_tenant_name: Option<String>,
    pub foreign_workspace_id: Option<String>,
    pub foreign_user_id: Option<String>,
    pub foreign_user_email: Option<String>,
    pub foreign_roles: Vec<String>,
    pub scope: serde_json::Value,
    pub status: BindingStatus,
    pub whoami_refreshed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
