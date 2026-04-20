use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "actor_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    System,
    Peer,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub actor_kind: ActorKind,
    pub actor_ref: String,
    pub verb: String,
    pub object_kind: String,
    pub object_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
