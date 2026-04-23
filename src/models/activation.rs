use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ActivationTrack {
    DemoWalkthrough,
    RealActivation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ActivationStepKey {
    AskedDemoQuestion,
    OpenedDemoSurvivor,
    ReviewedDemoApproval,
    ViewedDemoAudit,
    AddedConnector,
    FirstSignal,
    FirstApprovalDecided,
    FirstAuditViewed,
}

#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ActivationProgress {
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    pub track: ActivationTrack,
    pub step_key: ActivationStepKey,
    pub completed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ActivationDismissal {
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    pub track: ActivationTrack,
    pub dismissed_at: DateTime<Utc>,
}
