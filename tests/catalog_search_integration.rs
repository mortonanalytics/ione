//! Integration tests for federated catalog search
//! (md/plans/federated-catalog-search-plan.md). All three phases share this
//! file; tests are named `phase{N}_…` so each phase gate can run its slice.
//!
//! Run: IONE_SKIP_LIVE=1 cargo test --test catalog_search_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::Utc;
use ione::repos::PeerRepo;
use ione::services::federation::{reindex_peer_catalog, PeerManifest};
use ione::state::AppState;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase-catalog-search-test-bearer";

async fn spawn_app() -> (String, PgPool, AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    sqlx::query(
        "TRUNCATE peer_catalog_entries, webhook_events_seen, workspace_peer_bindings,
                  audit_events, pipeline_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let (app, state) = ione::app_with_state(pool.clone()).await;
    let state_ret = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool, state_ret)
}

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations ORDER BY created_at LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org")
}

/// Insert an `active` peer with a fixed `tool_prefix` in the default org and
/// return the loaded `Peer`. `org_id` is set by the peers org-id trigger.
async fn seed_peer(pool: &PgPool, name: &str, prefix: &str) -> ione::models::Peer {
    let org_id = default_org_id(pool).await;
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'mcp', 'local', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://issuer-{prefix}.example.com"))
    .fetch_one(pool)
    .await
    .expect("trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, status, tool_allowlist, tool_prefix)
         VALUES ($1, $2, $3, '{}'::jsonb, 'active'::peer_status, '[]'::jsonb, $4)
         RETURNING id",
    )
    .bind(name)
    .bind(format!("https://{prefix}.example.com"))
    .bind(issuer_id)
    .bind(prefix)
    .fetch_one(pool)
    .await
    .expect("peer");

    PeerRepo::new(pool.clone())
        .get(peer_id)
        .await
        .expect("get peer")
        .expect("peer exists")
}

fn tool(name: &str, description: &str, props: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> = props
        .iter()
        .map(|p| (p.to_string(), json!({ "type": "string" })))
        .collect();
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties }
    })
}

fn manifest(peer_id: Uuid, tools: Vec<Value>) -> PeerManifest {
    PeerManifest {
        peer_id,
        tools,
        resources: vec![],
        fetched_at: Utc::now(),
        etag: None,
        stale: false,
    }
}

// ── Phase 1 ────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn phase1_index_from_manifest() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &["lat", "lon"]),
            tool("get_alerts", "weather alerts", &["zone"]),
            tool("get_radar", "radar imagery", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m)
        .await
        .expect("reindex");

    let rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT namespaced_name, tsv IS NOT NULL AS has_tsv
         FROM peer_catalog_entries
         WHERE org_id = $1 AND peer_id = $2 AND kind = 'tool'
         ORDER BY namespaced_name",
    )
    .bind(peer.org_id)
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("query rows");

    assert_eq!(rows.len(), 3, "expected 3 tool rows");
    for (namespaced_name, has_tsv) in &rows {
        assert!(has_tsv, "tsv must be populated for {namespaced_name}");
        assert!(
            namespaced_name.starts_with("weatherpeer:"),
            "namespaced_name must be <tool_prefix>:<raw>, got {namespaced_name}"
        );
    }
    // Exact match to the prefix:name route_tool_call splits on.
    assert!(rows.iter().any(|(n, _)| n == "weatherpeer:get_forecast"));
}

#[tokio::test]
#[ignore]
async fn phase1_delta_no_churn() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m1 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &["lat"]),
            tool("get_alerts", "weather alerts", &["zone"]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 1");

    let before: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("before");

    // Re-index with identical content: no row's updated_at advances.
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 2");
    let after_same: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("after same");
    assert_eq!(
        before, after_same,
        "unchanged manifest must not bump updated_at"
    );

    // Change exactly one tool's description: only that row advances.
    let m2 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast UPDATED", &["lat"]),
            tool("get_alerts", "weather alerts", &["zone"]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m2)
        .await
        .expect("reindex 3");
    let after_change: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT namespaced_name, updated_at FROM peer_catalog_entries
         WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("after change");

    let forecast_before = before
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_forecast")
        .unwrap()
        .1;
    let forecast_after = after_change
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_forecast")
        .unwrap()
        .1;
    let alerts_before = before
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_alerts")
        .unwrap()
        .1;
    let alerts_after = after_change
        .iter()
        .find(|(n, _)| n == "weatherpeer:get_alerts")
        .unwrap()
        .1;
    assert!(forecast_after > forecast_before, "changed row must advance");
    assert_eq!(
        alerts_before, alerts_after,
        "unchanged row must not advance"
    );
}

#[tokio::test]
#[ignore]
async fn phase1_orphan_delete() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let m1 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &[]),
            tool("get_alerts", "weather alerts", &[]),
            tool("get_radar", "radar imagery", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m1)
        .await
        .expect("reindex 1");

    // Drop get_radar.
    let m2 = manifest(
        peer.id,
        vec![
            tool("get_forecast", "weather forecast", &[]),
            tool("get_alerts", "weather alerts", &[]),
        ],
    );
    reindex_peer_catalog(&state, &peer, &m2)
        .await
        .expect("reindex 2");

    let names: Vec<String> = sqlx::query_scalar(
        "SELECT namespaced_name FROM peer_catalog_entries WHERE peer_id = $1 ORDER BY namespaced_name",
    )
    .bind(peer.id)
    .fetch_all(&pool)
    .await
    .expect("names");
    assert_eq!(
        names,
        vec![
            "weatherpeer:get_alerts".to_string(),
            "weatherpeer:get_forecast".to_string()
        ],
        "orphaned tool must be deleted, survivors retained"
    );
}

#[tokio::test]
#[ignore]
async fn phase1_sanitize_stored() {
    let (_base, pool, state) = spawn_app().await;
    let peer = seed_peer(&pool, "Weather Peer", "weatherpeer").await;
    let dirty = format!(
        "<<<IONE_PEER_SLICE injected <img onerror=alert(1)> {}",
        "x".repeat(700)
    );
    let m = manifest(peer.id, vec![tool("get_forecast", &dirty, &[])]);
    reindex_peer_catalog(&state, &peer, &m)
        .await
        .expect("reindex");

    let stored: String = sqlx::query_scalar(
        "SELECT description FROM peer_catalog_entries WHERE peer_id = $1 LIMIT 1",
    )
    .bind(peer.id)
    .fetch_one(&pool)
    .await
    .expect("stored description");

    assert!(
        !stored.contains("<<<IONE_PEER_SLICE"),
        "slice sentinel must be stripped"
    );
    assert!(
        stored.chars().count() <= 512,
        "stored description must be capped at 512 chars, got {}",
        stored.chars().count()
    );
}
