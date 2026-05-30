use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct StreamEventAggregateRepo {
    pool: PgPool,
}

impl StreamEventAggregateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn stream_in_workspace_org(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
    ) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                FROM streams s
                JOIN connectors c ON c.id = s.connector_id
                JOIN workspaces w ON w.id = c.workspace_id
                WHERE s.id = $1 AND c.workspace_id = $2 AND w.org_id = $3
             )",
        )
        .bind(stream_id)
        .bind(workspace_id)
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to check stream scope")?;

        Ok(exists)
    }

    pub async fn count_by_bucket(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        bucket: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let bucket_expr = bucket_expr(bucket);
        let sql = format!(
            "SELECT {bucket_expr} AS bucket_start,
                    (EXTRACT(EPOCH FROM {bucket_expr}) * 1000)::bigint AS bucket_start_ms,
                    COUNT(*)::bigint AS event_count
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.workspace_id = $1
               AND w.org_id = $2
               AND se.stream_id = $3
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             GROUP BY bucket_start, bucket_start_ms
             ORDER BY bucket_start"
        );

        let rows = sqlx::query(&sql)
            .bind(workspace_id)
            .bind(org_id)
            .bind(stream_id)
            .bind(since)
            .bind(until)
            .fetch_all(&self.pool)
            .await
            .context("failed to aggregate counts by bucket")?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let bucket_start: DateTime<Utc> = row.get("bucket_start");
                let event_count: i64 = row.get("event_count");
                json!({
                    "bucketStart": bucket_start,
                    "bucketStartMs": row.get::<i64, _>("bucket_start_ms"),
                    "value": event_count,
                    "eventCount": event_count
                })
            })
            .collect())
    }

    pub async fn numeric_agg_by_bucket(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        bucket: &str,
        value_path: Vec<String>,
    ) -> anyhow::Result<Vec<Value>> {
        let bucket_expr = bucket_expr(bucket);
        let sql = format!(
            "SELECT {bucket_expr} AS bucket_start,
                    (EXTRACT(EPOCH FROM {bucket_expr}) * 1000)::bigint AS bucket_start_ms,
                    COUNT(*)::bigint AS event_count,
                    COUNT(*) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number')::bigint AS valid_count,
                    AVG((se.payload #>> $6)::double precision) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number') AS avg,
                    MAX((se.payload #>> $6)::double precision) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number') AS max,
                    MIN((se.payload #>> $6)::double precision) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number') AS min,
                    SUM((se.payload #>> $6)::double precision) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number') AS sum
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.workspace_id = $1
               AND w.org_id = $2
               AND se.stream_id = $3
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             GROUP BY bucket_start, bucket_start_ms
             ORDER BY bucket_start"
        );

        let rows = sqlx::query(&sql)
            .bind(workspace_id)
            .bind(org_id)
            .bind(stream_id)
            .bind(since)
            .bind(until)
            .bind(value_path)
            .fetch_all(&self.pool)
            .await
            .context("failed to aggregate numeric events by bucket")?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let bucket_start: DateTime<Utc> = row.get("bucket_start");
                json!({
                    "bucketStart": bucket_start,
                    "bucketStartMs": row.get::<i64, _>("bucket_start_ms"),
                    "eventCount": row.get::<i64, _>("event_count"),
                    "validCount": row.get::<i64, _>("valid_count"),
                    "avg": row.get::<Option<f64>, _>("avg"),
                    "max": row.get::<Option<f64>, _>("max"),
                    "min": row.get::<Option<f64>, _>("min"),
                    "sum": row.get::<Option<f64>, _>("sum")
                })
            })
            .collect())
    }

    pub async fn percentile_by_bucket(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        bucket: &str,
        value_path: Vec<String>,
        pct: f64,
    ) -> anyhow::Result<Vec<Value>> {
        let bucket_expr = bucket_expr(bucket);
        let sql = format!(
            "SELECT {bucket_expr} AS bucket_start,
                    (EXTRACT(EPOCH FROM {bucket_expr}) * 1000)::bigint AS bucket_start_ms,
                    COUNT(*)::bigint AS event_count,
                    COUNT(*) FILTER (WHERE jsonb_typeof(se.payload #> $6) = 'number')::bigint AS valid_count,
                    percentile_cont($7) WITHIN GROUP (
                        ORDER BY CASE
                            WHEN jsonb_typeof(se.payload #> $6) = 'number'
                            THEN (se.payload #>> $6)::double precision
                            ELSE NULL
                        END
                    ) AS percentile_value
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.workspace_id = $1
               AND w.org_id = $2
               AND se.stream_id = $3
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             GROUP BY bucket_start, bucket_start_ms
             ORDER BY bucket_start"
        );

        let rows = sqlx::query(&sql)
            .bind(workspace_id)
            .bind(org_id)
            .bind(stream_id)
            .bind(since)
            .bind(until)
            .bind(value_path)
            .bind(pct)
            .fetch_all(&self.pool)
            .await
            .context("failed to aggregate percentile by bucket")?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let bucket_start: DateTime<Utc> = row.get("bucket_start");
                json!({
                    "bucketStart": bucket_start,
                    "bucketStartMs": row.get::<i64, _>("bucket_start_ms"),
                    "eventCount": row.get::<i64, _>("event_count"),
                    "validCount": row.get::<i64, _>("valid_count"),
                    "percentileValue": row.get::<Option<f64>, _>("percentile_value")
                })
            })
            .collect())
    }

    pub async fn count_by_group(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        group_path: Vec<String>,
    ) -> anyhow::Result<(Vec<Value>, bool)> {
        let rows = sqlx::query(
            "SELECT COALESCE(NULLIF(se.payload #>> $6, ''), 'Unknown') AS group_key,
                    COUNT(*)::bigint AS event_count
             FROM stream_events se
             JOIN streams s ON s.id = se.stream_id
             JOIN connectors c ON c.id = s.connector_id
             JOIN workspaces w ON w.id = c.workspace_id
             WHERE c.workspace_id = $1
               AND w.org_id = $2
               AND se.stream_id = $3
               AND se.observed_at >= $4
               AND se.observed_at <= $5
             GROUP BY group_key
             ORDER BY event_count DESC, group_key ASC
             LIMIT 201",
        )
        .bind(workspace_id)
        .bind(org_id)
        .bind(stream_id)
        .bind(since)
        .bind(until)
        .bind(group_path)
        .fetch_all(&self.pool)
        .await
        .context("failed to aggregate counts by group")?;

        let truncated = rows.len() > 200;
        let rows = rows
            .into_iter()
            .take(200)
            .map(|row| {
                json!({
                    "groupKey": row.get::<String, _>("group_key"),
                    "eventCount": row.get::<i64, _>("event_count")
                })
            })
            .collect();
        Ok((rows, truncated))
    }

    pub async fn rolling_baseline(
        &self,
        workspace_id: Uuid,
        org_id: Uuid,
        stream_id: Uuid,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Value>> {
        let seed_since = since - Duration::days(30);
        let rows = self
            .count_by_bucket(workspace_id, org_id, stream_id, seed_since, until, "day")
            .await?;
        let mut counts = Vec::with_capacity(rows.len());
        for row in rows {
            let bucket_start: DateTime<Utc> =
                serde_json::from_value(row["bucketStart"].clone()).context("bucketStart")?;
            let event_count = row["eventCount"].as_i64().unwrap_or(0);
            counts.push((
                bucket_start,
                row["bucketStartMs"].as_i64().unwrap_or(0),
                event_count,
            ));
        }

        let mut out = Vec::new();
        for (idx, (bucket_start, bucket_start_ms, event_count)) in counts.iter().enumerate() {
            if *bucket_start < since {
                continue;
            }
            let window_start = *bucket_start - Duration::days(30);
            let window: Vec<i64> = counts[..=idx]
                .iter()
                .filter(|(day, _, _)| *day >= window_start)
                .map(|(_, _, count)| *count)
                .collect();
            let trailing_30d_avg = if window.len() < 2 {
                None
            } else {
                Some(window.iter().sum::<i64>() as f64 / window.len() as f64)
            };
            out.push(json!({
                "bucketStart": bucket_start,
                "bucketStartMs": bucket_start_ms,
                "value": event_count,
                "eventCount": event_count,
                "trailing30dAvg": trailing_30d_avg
            }));
        }
        Ok(out)
    }
}

fn bucket_expr(bucket: &str) -> &'static str {
    match bucket {
        "hour" => "date_trunc('hour', se.observed_at)",
        "day" => "date_trunc('day', se.observed_at)",
        "week" => "date_trunc('week', se.observed_at)",
        // `bucket` is interpolated into SQL; callers must validate it against the
        // allow-list (route layer) first. Fail loudly rather than silently default.
        other => panic!("bucket '{other}' must be validated before bucket_expr"),
    }
}
