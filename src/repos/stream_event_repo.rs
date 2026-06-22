use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::models::StreamEvent;
use crate::services::event_layers::{GeoEventRow, GeoStreamRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    Updated,
    Duplicate,
    Rejected,
}

/// Default cap on a single stream-event payload (serialized JSON bytes). Stream events
/// are inlined into generator/critic LLM prompts and stored unbounded as JSONB, so an
/// oversized peer-supplied payload can blow the context window or bloat storage. Mirrors
/// the `interaction_events` detail cap. Override with `IONE_MAX_STREAM_EVENT_BYTES`.
const DEFAULT_MAX_STREAM_EVENT_BYTES: usize = 65_536;

fn max_stream_event_bytes() -> usize {
    std::env::var("IONE_MAX_STREAM_EVENT_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_STREAM_EVENT_BYTES)
}

/// True if the payload serializes within the configured byte cap.
fn payload_within_limit(payload: &Value) -> bool {
    serde_json::to_vec(payload)
        .map(|v| v.len() <= max_stream_event_bytes())
        .unwrap_or(false)
}

pub struct StreamEventRepo {
    pub(crate) pool: PgPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortTarget {
    ObservedAt,
    Field(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct TableQuery {
    pub page: i64,
    pub per_page: i64,
    pub sort: SortTarget,
    pub sort_desc: bool,
    pub filter: Option<(SortTarget, String)>,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
}

impl StreamEventRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a stream event only if no row with the same (stream_id, observed_at) exists.
    /// Returns true if a row was inserted, false if it was a duplicate.
    pub async fn insert_if_absent(
        &self,
        stream_id: Uuid,
        payload: serde_json::Value,
        observed_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        if !payload_within_limit(&payload) {
            return Ok(false);
        }
        let rows_affected = sqlx::query(
            "INSERT INTO stream_events (stream_id, payload, observed_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (stream_id, observed_at) DO NOTHING",
        )
        .bind(stream_id)
        .bind(payload)
        .bind(observed_at)
        .execute(&self.pool)
        .await
        .context("failed to insert stream event")?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    pub async fn insert_event(
        &self,
        stream_id: Uuid,
        payload: serde_json::Value,
        observed_at: DateTime<Utc>,
        dedup_key: Option<&str>,
    ) -> anyhow::Result<InsertOutcome> {
        if !payload_within_limit(&payload) {
            return Ok(InsertOutcome::Rejected);
        }
        if let Some(dedup_key) = dedup_key {
            let inserted: bool = sqlx::query_scalar(
                "INSERT INTO stream_events (stream_id, payload, observed_at, dedup_key)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (stream_id, dedup_key) WHERE dedup_key IS NOT NULL
                 DO UPDATE SET payload = EXCLUDED.payload, observed_at = EXCLUDED.observed_at
                 RETURNING (xmax = 0) AS inserted",
            )
            .bind(stream_id)
            .bind(payload)
            .bind(observed_at)
            .bind(dedup_key)
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert stream event by dedup key")?;

            return Ok(if inserted {
                InsertOutcome::Inserted
            } else {
                InsertOutcome::Updated
            });
        }

        if self
            .insert_if_absent(stream_id, payload, observed_at)
            .await?
        {
            Ok(InsertOutcome::Inserted)
        } else {
            Ok(InsertOutcome::Duplicate)
        }
    }

    pub async fn list_recent(
        &self,
        stream_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<StreamEvent>> {
        sqlx::query_as::<_, StreamEvent>(
            "SELECT id, stream_id, payload, observed_at, ingested_at, embedding
             FROM stream_events
             WHERE stream_id = $1
             ORDER BY observed_at DESC
             LIMIT $2",
        )
        .bind(stream_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list recent stream events")
    }

    /// Fetch the catalog of geo-mapped streams in a workspace (Q1) and the
    /// events for those streams within the window (Q2, `LIMIT + 1` for
    /// unambiguous truncation detection). Both queries are org-scoped via the
    /// same workspace∈org fence used by `/map-layers`.
    pub async fn fetch_geo_events(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Option<Uuid>,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<(Vec<GeoStreamRow>, Vec<GeoEventRow>)> {
        let catalog = sqlx::query_as::<_, GeoStreamRow>(
            "SELECT s.id AS stream_id, s.name AS stream_name, s.view_config
             FROM streams s
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
               AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
               AND s.view_config IS NOT NULL
               AND ($3::uuid IS NULL OR s.id = $3)
             ORDER BY s.name",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(stream_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch geo-mapped stream catalog")?;

        let events = sqlx::query_as::<_, GeoEventRow>(
            "SELECT se.id AS event_id, se.stream_id, se.payload, se.observed_at
             FROM stream_events se
             JOIN streams s    ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             WHERE c.workspace_id = $1
               AND EXISTS (SELECT 1 FROM workspaces w WHERE w.id = $1 AND w.org_id = $2)
               AND s.view_config IS NOT NULL
               AND ($3::uuid IS NULL OR s.id = $3)
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             ORDER BY se.observed_at DESC
             LIMIT $6 + 1",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(stream_id)
        .bind(since)
        .bind(until)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch geo events")?;

        Ok((catalog, events))
    }

    pub async fn latest_observed_at(
        &self,
        stream_id: Uuid,
    ) -> anyhow::Result<Option<DateTime<Utc>>> {
        sqlx::query_scalar(
            "SELECT observed_at
             FROM stream_events
             WHERE stream_id = $1
             ORDER BY observed_at DESC
             LIMIT 1",
        )
        .bind(stream_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch latest observed_at for stream")
    }

    pub async fn fetch_table_rows(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        q: &TableQuery,
    ) -> anyhow::Result<(Vec<(Value, DateTime<Utc>)>, i64)> {
        let offset = (q.page - 1) * q.per_page;
        let sort_dir = if q.sort_desc { "DESC" } else { "ASC" };
        let mut sql = String::from(
            "SELECT se.payload, se.observed_at, count(*) OVER() AS total
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.workspace_id = $1
               AND w.org_id = $2
               AND se.stream_id = $3
               AND se.observed_at >= $4
               AND se.observed_at <= $5",
        );

        let mut next_param = 6;
        let filter_is_field = if let Some((target, _)) = &q.filter {
            match target {
                SortTarget::ObservedAt => {
                    let value_param = next_param;
                    next_param += 1;
                    sql.push_str(&format!(
                        " AND se.observed_at = ${value_param}::timestamptz"
                    ));
                    false
                }
                SortTarget::Field(_) => {
                    let path_param = next_param;
                    next_param += 1;
                    let value_param = next_param;
                    next_param += 1;
                    sql.push_str(&format!(
                        " AND se.payload #>> ${path_param}::text[] ILIKE '%' || ${value_param} || '%'"
                    ));
                    true
                }
            }
        } else {
            false
        };

        let sort_param = match &q.sort {
            SortTarget::ObservedAt => {
                sql.push_str(&format!(" ORDER BY se.observed_at {sort_dir} NULLS LAST"));
                None
            }
            SortTarget::Field(_) => {
                let current = next_param;
                next_param += 1;
                sql.push_str(&format!(
                    " ORDER BY CASE
                          WHEN jsonb_typeof(se.payload #> ${current}::text[]) = 'number'
                          THEN (se.payload #>> ${current}::text[])::double precision
                          ELSE NULL
                      END {sort_dir} NULLS LAST,
                      se.payload #>> ${current}::text[] {sort_dir} NULLS LAST"
                ));
                Some(current)
            }
        };

        let limit_param = next_param;
        next_param += 1;
        let offset_param = next_param;
        sql.push_str(&format!(" LIMIT ${limit_param} OFFSET ${offset_param}"));

        let mut query = sqlx::query(&sql)
            .bind(workspace_id)
            .bind(org_id)
            .bind(stream_id)
            .bind(q.since)
            .bind(q.until);

        if let Some((target, value)) = &q.filter {
            if filter_is_field {
                if let SortTarget::Field(path) = target {
                    query = query.bind(path.clone()).bind(value);
                }
            } else {
                query = query.bind(value);
            }
        }

        if sort_param.is_some() {
            if let SortTarget::Field(path) = &q.sort {
                query = query.bind(path.clone());
            }
        }

        let rows = query
            .bind(q.per_page)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .context("failed to fetch table rows")?;

        let total = rows
            .first()
            .map(|row| row.get::<i64, _>("total"))
            .unwrap_or(0);
        let projected = rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<Value, _>("payload"),
                    row.get::<DateTime<Utc>, _>("observed_at"),
                )
            })
            .collect();

        Ok((projected, total))
    }
}
