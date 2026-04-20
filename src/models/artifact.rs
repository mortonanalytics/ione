use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "artifact_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Briefing,
    NotificationDraft,
    ResourceOrder,
    Message,
    Report,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: ArtifactKind,
    pub source_survivor_id: Option<Uuid>,
    pub content: serde_json::Value,
    pub blob_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}
