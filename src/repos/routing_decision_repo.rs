use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{RoutingDecision, RoutingTarget, SurvivorRow};

pub struct RoutingDecisionRepo {
    pub pool: PgPool,
}

impl RoutingDecisionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        survivor_id: Uuid,
        target_kind: RoutingTarget,
        target_ref: serde_json::Value,
        classifier_model: &str,
        rationale: &str,
    ) -> anyhow::Result<RoutingDecision> {
        sqlx::query_as::<_, RoutingDecision>(
            "INSERT INTO routing_decisions
               (survivor_id, target_kind, target_ref, classifier_model, rationale)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, survivor_id, target_kind, target_ref, classifier_model, rationale, created_at",
        )
        .bind(survivor_id)
        .bind(target_kind)
        .bind(target_ref)
        .bind(classifier_model)
        .bind(rationale)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert routing_decision")
    }

    pub async fn list_for_survivor(
        &self,
        survivor_id: Uuid,
    ) -> anyhow::Result<Vec<RoutingDecision>> {
        sqlx::query_as::<_, RoutingDecision>(
            "SELECT id, survivor_id, target_kind, target_ref, classifier_model, rationale, created_at
             FROM routing_decisions
             WHERE survivor_id = $1
             ORDER BY created_at ASC",
        )
        .bind(survivor_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list routing_decisions for survivor")
    }

    pub async fn exists_for_survivor(&self, survivor_id: Uuid) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM routing_decisions WHERE survivor_id = $1)",
        )
        .bind(survivor_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to check routing_decision existence for survivor")?;

        Ok(exists)
    }

    /// Return survivors with a feed routing_decision targeting the given role_id,
    /// joined with their parent signal for display context.
    /// Ordered by survivors.created_at DESC.
    pub async fn feed_for_role(
        &self,
        workspace_id: Uuid,
        role_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<SurvivorRow>> {
        sqlx::query_as::<_, SurvivorRow>(
            "SELECT s.id, s.signal_id, s.critic_model, s.verdict, s.rationale,
                    s.confidence, s.chain_of_reasoning, s.created_at,
                    sig.title AS signal_title, sig.body AS signal_body,
                    sig.severity AS signal_severity
             FROM survivors s
             JOIN signals sig ON sig.id = s.signal_id
             WHERE sig.workspace_id = $1
               AND EXISTS (
                   SELECT 1 FROM routing_decisions rd
                   WHERE rd.survivor_id = s.id
                     AND rd.target_kind = 'feed'::routing_target
                     AND rd.target_ref->>'role_id' = $2::text
               )
             ORDER BY s.created_at DESC
             LIMIT $3",
        )
        .bind(workspace_id)
        .bind(role_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to query feed for role")
    }
}
