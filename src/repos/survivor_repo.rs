use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{CriticVerdict, Survivor, SurvivorRow};

pub struct SurvivorRepo {
    pub pool: PgPool,
}

impl SurvivorRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        signal_id: Uuid,
        critic_model: &str,
        verdict: CriticVerdict,
        rationale: &str,
        confidence: f32,
        chain_of_reasoning: serde_json::Value,
    ) -> anyhow::Result<Survivor> {
        sqlx::query_as::<_, Survivor>(
            "INSERT INTO survivors
               (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, signal_id, critic_model, verdict, rationale, confidence,
                       chain_of_reasoning, created_at",
        )
        .bind(signal_id)
        .bind(critic_model)
        .bind(verdict)
        .bind(rationale)
        .bind(confidence)
        .bind(chain_of_reasoning)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert survivor")
    }

    /// List survivors for a workspace, joined with parent signal for display context.
    /// Ordered by survivor.created_at DESC.
    pub async fn list(
        &self,
        workspace_id: Uuid,
        verdict_filter: Option<CriticVerdict>,
        limit: i64,
    ) -> anyhow::Result<Vec<SurvivorRow>> {
        match verdict_filter {
            Some(v) => sqlx::query_as::<_, SurvivorRow>(
                "SELECT s.id, s.signal_id, s.critic_model, s.verdict, s.rationale,
                            s.confidence, s.chain_of_reasoning, s.created_at,
                            sig.title AS signal_title, sig.body AS signal_body,
                            sig.severity AS signal_severity
                     FROM survivors s
                     JOIN signals sig ON sig.id = s.signal_id
                     WHERE sig.workspace_id = $1
                       AND s.verdict = $2
                     ORDER BY s.created_at DESC
                     LIMIT $3",
            )
            .bind(workspace_id)
            .bind(v)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .context("failed to list survivors with verdict filter"),
            None => sqlx::query_as::<_, SurvivorRow>(
                "SELECT s.id, s.signal_id, s.critic_model, s.verdict, s.rationale,
                            s.confidence, s.chain_of_reasoning, s.created_at,
                            sig.title AS signal_title, sig.body AS signal_body,
                            sig.severity AS signal_severity
                     FROM survivors s
                     JOIN signals sig ON sig.id = s.signal_id
                     WHERE sig.workspace_id = $1
                     ORDER BY s.created_at DESC
                     LIMIT $2",
            )
            .bind(workspace_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .context("failed to list survivors"),
        }
    }

    /// Returns true if a survivor already exists for the given signal_id.
    pub async fn exists_for_signal(&self, signal_id: Uuid) -> anyhow::Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM survivors WHERE signal_id = $1)")
                .bind(signal_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to check survivor existence for signal")?;

        Ok(exists)
    }
}
