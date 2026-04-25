/// Phase 13 — two-node fire-ops demo test.
///
/// Scope: verifies the full peer-delivery end-to-end path using the Phase 13
/// connectors as context.  The signal/generator/critic/router stages are NOT
/// re-tested here (those have their own phase tests).  We seed a pre-existing
/// signal + survivor with an explicit `peer` routing_decision and assert that
/// the delivery reaches Node B as a pending artifact/approval.
///
/// Ollama-gated:
///   - IONE_SKIP_LIVE=1  — skips live Ollama; generator/critic/router use their
///     fallback paths.  This test does NOT call the scheduler; it seeds all
///     pipeline stages directly so Ollama is not needed.
///
/// Run:
///   IONE_SKIP_LIVE=1 DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase13_demo -- --ignored --test-threads=1
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "phase13-demo-peer-bearer";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
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

    truncate_all(&pool).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind node A");
    let addr: SocketAddr = listener.local_addr().expect("addr A");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server A error") });

    (format!("http://{}", addr), pool)
}

async fn spawn_second_app() -> (String, PgPool) {
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pool B connect failed");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration B failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind node B");
    let addr: SocketAddr = listener.local_addr().expect("addr B");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server B error") });

    (format!("http://{}", addr), pool)
}

async fn truncate_all(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .expect("truncate failed");
}

// ─── Seed helpers ─────────────────────────────────────────────────────────────

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Default Org not found")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn insert_trust_issuer(pool: &PgPool, org_id: Uuid, issuer_url: &str) -> Uuid {
    use base64::Engine as _;
    let secret: Vec<u8> = (0u8..32).collect();
    let secret_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&secret);
    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', $3, '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .bind(format!("secret:{}", secret_b64))
    .fetch_one(pool)
    .await
    .expect("insert trust_issuer")
}

async fn insert_peer(pool: &PgPool, name: &str, mcp_url: &str, issuer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy, tool_allowlist)
         VALUES ($1, $2, $3, '{}'::jsonb, '[\"propose_artifact\"]'::jsonb)
         RETURNING id",
    )
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer")
}

async fn insert_firms_connector(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'firms-lolo',
                 '{\"kind\":\"firms\",\"map_key\":\"DEMO_KEY\",\"area\":\"MONTANA\"}'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert firms connector")
}

async fn insert_signal(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, 'FIRMS High Hotspot Count: Lolo NF',
                 'VIIRS detected 5 hotspots in the Lolo NF area in the last 24h.',
                 'flagged'::severity, '[{\"source\":\"firms\",\"event_count\":5}]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("insert signal")
}

async fn insert_survivor(pool: &PgPool, signal_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'phi4-reasoning:14b', 'survive'::critic_verdict,
                 'Multiple VIIRS hotspots indicate active fire requiring coordination.',
                 0.92, '[\"FIRMS hotspot density threshold exceeded\"]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(pool)
    .await
    .expect("insert survivor")
}

async fn insert_routing_decision(pool: &PgPool, survivor_id: Uuid, peer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, 'peer'::routing_target, $2, 'test', 'Multi-node coordination required')
         RETURNING id",
    )
    .bind(survivor_id)
    .bind(json!({ "peer_id": peer_id }))
    .fetch_one(pool)
    .await
    .expect("insert routing_decision")
}

// ─── Main demo test ───────────────────────────────────────────────────────────

/// Two-node fire-ops demo: Node A seeds a FIRMS-derived signal with peer routing
/// to Node B; delivery calls propose_artifact on B; Node B has a pending approval.
///
/// This does NOT re-test the scheduler, generator, critic, or router.
/// It seeds those pipeline outputs directly so the test is deterministic without
/// Ollama.  The only live assertion is: peer delivery end-to-end works.
#[tokio::test]
#[ignore]
async fn two_node_demo_via_http_harness() {
    // Node A truncates + bootstraps.
    let (base_a, pool_a) = spawn_app().await;

    // Node B bootstraps (no truncate).
    let (base_b, _pool_b) = spawn_second_app().await;

    let org_id = default_org_id(&pool_a).await;
    let ws_a = default_workspace_id(&pool_a).await;

    // ── Register FIRMS connector on Node A ────────────────────────────────────
    let _firms_id = insert_firms_connector(&pool_a, ws_a).await;

    // ── Create trust_issuer + peer pointing at Node B's MCP ──────────────────
    let issuer_id = insert_trust_issuer(&pool_a, org_id, "https://iss-demo13.test").await;

    let peer_mcp_url = format!("{}/mcp", base_b);
    let peer_id = insert_peer(&pool_a, "Node B — Lolo NF", &peer_mcp_url, issuer_id).await;

    // ── Create MCP connector in workspace A pointing at Node B ───────────────
    sqlx::query(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:Node B — Lolo NF',
                 jsonb_build_object('mcp_url', $2, 'bearer_token', $3))",
    )
    .bind(ws_a)
    .bind(&peer_mcp_url)
    .bind(TEST_STATIC_BEARER)
    .execute(&pool_a)
    .await
    .expect("insert mcp connector");

    // ── Seed signal → survivor → routing_decision ─────────────────────────────
    let signal_id = insert_signal(&pool_a, ws_a).await;
    let survivor_id = insert_survivor(&pool_a, signal_id).await;
    let routing_id = insert_routing_decision(&pool_a, survivor_id, peer_id).await;

    // ── Count artifacts before delivery ──────────────────────────────────────
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count before");

    // ── Run delivery on Node A ────────────────────────────────────────────────
    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool_a.clone(), Uuid::nil(), ws_a);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision failed");

    // ── Assert: a new artifact arrived (peer proposed to Node B) ─────────────
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count after");

    assert!(
        after > before,
        "an artifact must exist after peer delivery (before={}, after={})",
        before,
        after
    );

    // ── Assert: audit verb=peer_delivered on Node A ───────────────────────────
    let audit_verb: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events
         WHERE workspace_id = $1 AND verb = 'peer_delivered'
         LIMIT 1",
    )
    .bind(ws_a)
    .fetch_optional(&pool_a)
    .await
    .expect("audit query");

    assert_eq!(
        audit_verb.as_deref(),
        Some("peer_delivered"),
        "must record 'peer_delivered' audit on Node A"
    );

    // ── Assert via HTTP: Node B has ≥1 pending approval ──────────────────────
    // Brief wait for Node B's in-process bootstrap to settle.
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Get Operations workspace from Node B's DB (shared DB).
    let ws_b = default_workspace_id(&pool_a).await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/workspaces/{}/approvals?status=pending",
            base_b, ws_b
        ))
        .send()
        .await
        .expect("GET approvals on B failed");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "GET approvals on B must return 200, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response not JSON");
    let items = body["items"].as_array().expect("items array");

    // Node B received a peer-proposed artifact; it must have a pending approval.
    assert!(
        !items.is_empty(),
        "Node B must have ≥1 pending approval after peer delivery from A, got 0. \
         Artifacts total: {}",
        after
    );

    // ── Print audit trail via audit_events endpoint ────────────────────────────
    let audit_resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/workspaces/{}/audit_events",
            base_a, ws_a
        ))
        .send()
        .await
        .expect("GET audit_events failed");

    let audit_body: Value = audit_resp.json().await.expect("audit_events not JSON");
    let audit_items = audit_body["items"].as_array().cloned().unwrap_or_default();
    eprintln!("=== Node A audit trail ({} events) ===", audit_items.len());
    for ev in &audit_items {
        eprintln!(
            "  {} | {} | {}",
            ev["createdAt"], ev["verb"], ev["objectKind"]
        );
    }

    // ── Artifacts endpoint smoke-check ────────────────────────────────────────
    let artifacts_resp = reqwest::Client::new()
        .get(format!("{}/api/v1/workspaces/{}/artifacts", base_a, ws_a))
        .send()
        .await
        .expect("GET artifacts failed");

    let artifacts_body: Value = artifacts_resp.json().await.expect("artifacts not JSON");
    assert!(
        artifacts_body["items"].as_array().is_some(),
        "artifacts endpoint must return items array"
    );
}
