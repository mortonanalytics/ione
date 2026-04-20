use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub coc_level: i32,
    pub permissions: serde_json::Value,
}
