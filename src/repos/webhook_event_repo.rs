use anyhow::Context;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub struct WebhookEventRepo {
    pool: PgPool,
}

impl WebhookEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn try_insert_seen_tx(
        tx: &mut PgConnection,
        event_id: &str,
        peer_id: Uuid,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "INSERT INTO webhook_events_seen (event_id, peer_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(event_id)
        .bind(peer_id)
        .execute(tx)
        .await
        .context("failed to insert webhook dedup row")?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn cleanup_expired(&self) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "DELETE FROM webhook_events_seen
             WHERE received_at < now() - INTERVAL '72 hours'",
        )
        .execute(&self.pool)
        .await
        .context("failed to cleanup expired webhook dedup rows")?;
        Ok(result.rows_affected())
    }
}
