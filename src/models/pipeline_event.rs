use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PipelineEventStage {
    PublishStarted,
    FirstEvent,
    FirstSignal,
    FirstSurvivor,
    FirstDecision,
    Stall,
    Error,
}

impl PipelineEventStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PipelineEventStage::PublishStarted => "publish_started",
            PipelineEventStage::FirstEvent => "first_event",
            PipelineEventStage::FirstSignal => "first_signal",
            PipelineEventStage::FirstSurvivor => "first_survivor",
            PipelineEventStage::FirstDecision => "first_decision",
            PipelineEventStage::Stall => "stall",
            PipelineEventStage::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PipelineEvent {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub connector_id: Option<Uuid>,
    pub stream_id: Option<Uuid>,
    pub stage: PipelineEventStage,
    pub detail: Option<serde_json::Value>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct PipelineEventInput {
    pub workspace_id: Uuid,
    pub connector_id: Option<Uuid>,
    pub stream_id: Option<Uuid>,
    pub stage: PipelineEventStage,
    pub detail: Option<serde_json::Value>,
}
