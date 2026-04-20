use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "workspace_lifecycle", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceLifecycle {
    Continuous,
    Bounded,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: Uuid,
    pub org_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub domain: String,
    pub lifecycle: WorkspaceLifecycle,
    pub end_condition: Option<serde_json::Value>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}
