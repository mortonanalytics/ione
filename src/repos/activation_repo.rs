use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{ActivationProgress, ActivationStepKey, ActivationTrack};

pub struct ActivationRepo {
    pool: PgPool,
}

impl ActivationRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn mark(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        track: ActivationTrack,
        step: ActivationStepKey,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO activation_progress (user_id, workspace_id, track, step_key)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(track_str(track))
        .bind(step_key_str(step))
        .execute(&self.pool)
        .await
        .context("failed to mark activation progress")?;
        Ok(())
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        track: ActivationTrack,
    ) -> Result<Vec<ActivationProgress>> {
        sqlx::query_as::<_, ActivationProgress>(
            "SELECT user_id, workspace_id, track, step_key, completed_at
               FROM activation_progress
              WHERE user_id = $1
                AND workspace_id = $2
                AND track = $3
              ORDER BY completed_at",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(track_str(track))
        .fetch_all(&self.pool)
        .await
        .context("failed to list activation progress")
    }

    pub async fn is_dismissed(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        track: ActivationTrack,
    ) -> Result<bool> {
        let row: Option<i64> = sqlx::query_scalar(
            "SELECT 1
               FROM activation_dismissals
              WHERE user_id = $1
                AND workspace_id = $2
                AND track = $3",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(track_str(track))
        .fetch_optional(&self.pool)
        .await
        .context("failed to query activation dismissal state")?;
        Ok(row.is_some())
    }

    pub async fn dismiss(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        track: ActivationTrack,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO activation_dismissals (user_id, workspace_id, track)
             VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(track_str(track))
        .execute(&self.pool)
        .await
        .context("failed to dismiss activation track")?;
        Ok(())
    }
}

fn track_str(track: ActivationTrack) -> &'static str {
    match track {
        ActivationTrack::DemoWalkthrough => "demo_walkthrough",
        ActivationTrack::RealActivation => "real_activation",
    }
}

fn step_key_str(step: ActivationStepKey) -> &'static str {
    match step {
        ActivationStepKey::AskedDemoQuestion => "asked_demo_question",
        ActivationStepKey::OpenedDemoSurvivor => "opened_demo_survivor",
        ActivationStepKey::ReviewedDemoApproval => "reviewed_demo_approval",
        ActivationStepKey::ViewedDemoAudit => "viewed_demo_audit",
        ActivationStepKey::AddedConnector => "added_connector",
        ActivationStepKey::FirstSignal => "first_signal",
        ActivationStepKey::FirstApprovalDecided => "first_approval_decided",
        ActivationStepKey::FirstAuditViewed => "first_audit_viewed",
    }
}
