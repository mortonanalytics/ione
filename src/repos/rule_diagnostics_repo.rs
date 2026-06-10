use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{RuleDiagnostic, RuleDiagnosticSnapshot};

pub struct RuleDiagnosticsRepo {
    pool: PgPool,
}

impl RuleDiagnosticsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert(&self, workspace_id: Uuid, items: &[RuleDiagnostic]) -> anyhow::Result<()> {
        let items = serde_json::to_value(items).context("serialize rule diagnostics")?;
        sqlx::query(
            "INSERT INTO rule_diagnostics (workspace_id, evaluated_at, items)
             VALUES ($1, now(), $2)
             ON CONFLICT (workspace_id)
             DO UPDATE SET evaluated_at = now(), items = EXCLUDED.items",
        )
        .bind(workspace_id)
        .bind(items)
        .execute(&self.pool)
        .await
        .context("upsert rule diagnostics")?;
        Ok(())
    }

    pub async fn get(
        &self,
        workspace_id: Uuid,
    ) -> anyhow::Result<Option<(DateTime<Utc>, Vec<RuleDiagnostic>)>> {
        let row = sqlx::query_as::<_, RuleDiagnosticSnapshot>(
            "SELECT workspace_id, evaluated_at, items
             FROM rule_diagnostics
             WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .context("get rule diagnostics")?;

        row.map(|snap| {
            let items =
                serde_json::from_value(snap.items).context("deserialize rule diagnostics")?;
            Ok((snap.evaluated_at, items))
        })
        .transpose()
    }

    pub async fn clear(&self, workspace_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM rule_diagnostics WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&self.pool)
            .await
            .context("clear rule diagnostics")?;
        Ok(())
    }
}
