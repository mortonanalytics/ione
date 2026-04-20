use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    pub id: Uuid,
    pub stream_id: Uuid,
    pub payload: serde_json::Value,
    pub observed_at: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    #[serde(skip)]
    pub embedding: Option<pgvector::Vector>,
}
