use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub title: String,
    pub created_at: DateTime<Utc>,
}
