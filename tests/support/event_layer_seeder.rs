use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Seed a geo-mapped stream (connector + stream with `view_config`) plus its
/// events, bypassing the real connectors. Returns the new stream's id.
pub async fn seed_geo_stream(
    pool: &PgPool,
    workspace_id: Uuid,
    stream_name: &str,
    view_config: serde_json::Value,
    events: Vec<(serde_json::Value, DateTime<Utc>)>,
) -> Uuid {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config, status)
         VALUES ($1, 'rust_native'::connector_kind, $2, '{}'::jsonb, 'active'::connector_status)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("seed-{stream_name}"))
    .fetch_one(pool)
    .await
    .expect("insert connector");

    let stream_id: Uuid = sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name, schema, view_config)
         VALUES ($1, $2, '{}'::jsonb, $3)
         RETURNING id",
    )
    .bind(connector_id)
    .bind(stream_name)
    .bind(view_config)
    .fetch_one(pool)
    .await
    .expect("insert stream");

    for (payload, observed_at) in events {
        sqlx::query(
            "INSERT INTO stream_events (stream_id, payload, observed_at)
             VALUES ($1, $2, $3)",
        )
        .bind(stream_id)
        .bind(payload)
        .bind(observed_at)
        .execute(pool)
        .await
        .expect("insert stream event");
    }

    stream_id
}
