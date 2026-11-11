//! `?limit=` on the signals, survivors and feed list endpoints is clamped to
//! 1..=500. A negative limit used to reach Postgres as `LIMIT -1` and come back
//! as a 500 (#129).
//!
//! Run: cargo test --test list_limit_clamp_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// Boot the app on a clean database holding two signals, each with a survivor
/// routed to one role's feed. Returns `(base_url, workspace_id, role_id)`.
async fn spawn_seeded_app() -> (String, Uuid, Uuid) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");
    sqlx::query(
        "TRUNCATE routing_decisions, survivors, signals, stream_events, streams,
                  connectors, memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    let ws_id: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("Operations workspace not found");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, 'member', 1, '{}'::jsonb)
         RETURNING id",
    )
    .bind(ws_id)
    .fetch_one(&pool)
    .await
    .expect("insert role");

    for title in ["first", "second"] {
        let signal_id: Uuid = sqlx::query_scalar(
            "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
             VALUES ($1, 'rule'::signal_source, $2, 'test body', 'routine'::severity, '[]'::jsonb)
             RETURNING id",
        )
        .bind(ws_id)
        .bind(title)
        .fetch_one(&pool)
        .await
        .expect("insert signal");
        let survivor_id: Uuid = sqlx::query_scalar(
            "INSERT INTO survivors
               (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
             VALUES ($1, 'test-critic', 'survive'::critic_verdict, 'test rationale', 0.8, '[]'::jsonb)
             RETURNING id",
        )
        .bind(signal_id)
        .fetch_one(&pool)
        .await
        .expect("insert survivor");
        sqlx::query(
            "INSERT INTO routing_decisions
               (survivor_id, target_kind, target_ref, classifier_model, rationale)
             VALUES ($1, 'feed'::routing_target, $2, 'test-classifier', 'test rationale')",
        )
        .bind(survivor_id)
        .bind(json!({ "role_id": role_id }))
        .execute(&pool)
        .await
        .expect("insert routing decision");
    }

    (format!("http://{addr}"), ws_id, role_id)
}

/// `url` already carries its own query string or none; `sep` joins the limit on.
async fn assert_limit_is_clamped(url: &str, sep: char) {
    let client = reqwest::Client::new();
    for (limit, max_items) in [("-1", 1), ("0", 1), ("10000", 500)] {
        let resp = client
            .get(format!("{url}{sep}limit={limit}"))
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), StatusCode::OK, "limit={limit} on {url}");
        let body: Value = resp.json().await.expect("json");
        let items = body["items"].as_array().expect("items array");
        assert!(
            !items.is_empty() && items.len() <= max_items,
            "limit={limit} on {url} returned {} items",
            items.len()
        );
    }
}

#[tokio::test]
#[ignore]
async fn signals_clamps_limit() {
    let (base, ws_id, _) = spawn_seeded_app().await;
    assert_limit_is_clamped(&format!("{base}/api/v1/workspaces/{ws_id}/signals"), '?').await;
}

#[tokio::test]
#[ignore]
async fn survivors_clamps_limit() {
    let (base, ws_id, _) = spawn_seeded_app().await;
    assert_limit_is_clamped(&format!("{base}/api/v1/workspaces/{ws_id}/survivors"), '?').await;
}

#[tokio::test]
#[ignore]
async fn feed_clamps_limit() {
    let (base, ws_id, role_id) = spawn_seeded_app().await;
    assert_limit_is_clamped(
        &format!("{base}/api/v1/workspaces/{ws_id}/feed?roleId={role_id}"),
        '&',
    )
    .await;
}
