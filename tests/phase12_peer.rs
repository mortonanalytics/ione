/// Phase 12 contract tests — MCP client + peer federation.
///
/// Targets:
///   - Contract: md/design/ione-v1-contract.md  entity `peer`, enum `peer_status`
///   - Plan:     md/plans/ione-v1-plan.md        Phase 12
///
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run:
///   IONE_SKIP_LIVE=1 DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase12_peer -- --ignored --test-threads=1
///
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    truncate_all(&pool).await;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

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

/// Spawn a second app instance against the same DB without truncating.
/// Used in two-node federation tests where node A runs spawn_app() first.
async fn spawn_second_app() -> (String, PgPool) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pool B connect failed");

    // Run migrations (idempotent).
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration B failed");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind port B");
    let addr: SocketAddr = listener.local_addr().expect("failed to get addr B");

    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server B error");
    });

    (format!("http://{}", addr), pool)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

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

/// Insert a trust_issuer with an HMAC secret, return (issuer_id, secret_bytes).
async fn insert_trust_issuer(
    pool: &PgPool,
    org_id: Uuid,
    issuer_url: &str,
    audience: &str,
) -> (Uuid, Vec<u8>) {
    use base64::Engine as _;
    let secret: Vec<u8> = (0u8..32).collect();
    let secret_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&secret);
    let jwks_uri = format!("secret:{}", secret_b64);

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, $3, $4, '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .bind(audience)
    .bind(&jwks_uri)
    .fetch_one(pool)
    .await
    .expect("insert trust_issuer failed");

    (id, secret)
}

async fn insert_peer(pool: &PgPool, name: &str, mcp_url: &str, issuer_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy)
         VALUES ($1, $2, $3, '{}'::jsonb)
         RETURNING id",
    )
    .bind(name)
    .bind(mcp_url)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer failed")
}

async fn insert_workspace(pool: &PgPool, org_id: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert workspace failed")
}

async fn insert_signal(pool: &PgPool, workspace_id: Uuid, title: &str, severity: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO signals (workspace_id, source, title, body, severity, evidence)
         VALUES ($1, 'rule'::signal_source, $2, 'test body', $3::severity, '[]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(title)
    .bind(severity)
    .fetch_one(pool)
    .await
    .expect("insert signal failed")
}

async fn insert_survivor(pool: &PgPool, signal_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO survivors
           (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'phi4-reasoning:14b', 'survive'::critic_verdict,
                 'test rationale', 0.9, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(pool)
    .await
    .expect("insert survivor failed")
}

async fn insert_routing_decision(
    pool: &PgPool,
    survivor_id: Uuid,
    target_kind: &str,
    target_ref: Value,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO routing_decisions (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, $2::routing_target, $3, 'test', 'test')
         RETURNING id",
    )
    .bind(survivor_id)
    .bind(target_kind)
    .bind(target_ref)
    .fetch_one(pool)
    .await
    .expect("insert routing_decision failed")
}

fn mint_jwt(subject: &str, issuer: &str, audience: &str, secret: &[u8]) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let claims = json!({
        "sub": subject,
        "iss": issuer,
        "aud": audience,
        "exp": chrono::Utc::now().timestamp() + 3600,
        "iat": chrono::Utc::now().timestamp(),
    });
    let header = Header::new(Algorithm::HS256);
    encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("failed to mint JWT")
}

// ─── Test 1: peer_status_enum_variants ───────────────────────────────────────

/// peer_status has exactly the three variants: active, paused, error.
#[tokio::test]
#[ignore]
async fn peer_status_enum_variants() {
    let (_, pool) = spawn_app().await;

    let variants: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::peer_status))::TEXT")
            .fetch_all(&pool)
            .await
            .expect("failed to query peer_status enum variants");

    assert_eq!(
        variants.len(),
        3,
        "peer_status must have 3 variants, got: {:?}",
        variants
    );
    assert!(
        variants.contains(&"active".to_string()),
        "missing variant 'active'"
    );
    assert!(
        variants.contains(&"paused".to_string()),
        "missing variant 'paused'"
    );
    assert!(
        variants.contains(&"error".to_string()),
        "missing variant 'error'"
    );
}

// ─── Test 2: mcp_url_is_unique ────────────────────────────────────────────────

/// Inserting two peers with the same mcp_url must fail with a unique constraint error.
#[tokio::test]
#[ignore]
async fn mcp_url_is_unique() {
    let (_, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let (issuer_id, _) = insert_trust_issuer(&pool, org_id, "https://iss.test", "aud").await;

    insert_peer(&pool, "Peer A", "https://peer.example.com/mcp", issuer_id).await;

    // Second insert with same URL must fail.
    let result: Result<Uuid, _> = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy)
         VALUES ('Peer B', 'https://peer.example.com/mcp', $1, '{}'::jsonb)
         RETURNING id",
    )
    .bind(issuer_id)
    .fetch_one(&pool)
    .await;

    assert!(result.is_err(), "second insert with same mcp_url must fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unique") || err_msg.contains("duplicate"),
        "error must mention unique constraint, got: {}",
        err_msg
    );
}

// ─── Test 3: create_peer_requires_existing_issuer_id ─────────────────────────

/// POST /api/v1/peers with a non-existent issuer_id must return 4xx.
#[tokio::test]
#[ignore]
async fn create_peer_requires_existing_issuer_id() {
    let (base, _pool) = spawn_app().await;

    let bad_issuer_id = Uuid::new_v4();

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/peers", base))
        .json(&json!({
            "name": "Test Peer",
            "mcpUrl": "https://peer-bad.example.com/mcp",
            "issuerId": bad_issuer_id,
            "sharingPolicy": {}
        }))
        .send()
        .await
        .expect("POST /api/v1/peers failed");

    assert!(
        resp.status().is_client_error(),
        "non-existent issuer_id must return 4xx, got: {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        !body["error"].is_null(),
        "response must contain an error field"
    );
}

// ─── Test 4: subscribe_creates_mcp_connector_in_workspace ────────────────────

/// After POST subscribe, the workspace has a connector with kind=mcp and correct mcp_url.
#[tokio::test]
#[ignore]
async fn subscribe_creates_mcp_connector_in_workspace() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let (issuer_id, _) = insert_trust_issuer(&pool, org_id, "https://iss-sub.test", "aud").await;

    let mcp_url = "http://127.0.0.1:19999/mcp"; // unreachable is fine — just testing DB record
    let peer_id = insert_peer(&pool, "Sub Peer", mcp_url, issuer_id).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/workspaces/{}/peers/{}/subscribe",
            base, workspace_id, peer_id
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("POST subscribe failed");

    assert_eq!(resp.status(), StatusCode::OK, "subscribe must return 200");

    let body: Value = resp.json().await.expect("response not JSON");
    assert_eq!(
        body["kind"].as_str().unwrap_or(""),
        "mcp",
        "created connector must have kind=mcp, got: {}",
        body
    );

    // Verify in DB: one mcp connector with the correct mcp_url in config.
    let config_mcp_url: Option<String> = sqlx::query_scalar(
        "SELECT config->>'mcp_url' FROM connectors
         WHERE workspace_id = $1 AND kind = 'mcp'::connector_kind
         LIMIT 1",
    )
    .bind(workspace_id)
    .fetch_optional(&pool)
    .await
    .expect("connector query failed");

    assert_eq!(
        config_mcp_url.as_deref(),
        Some(mcp_url),
        "connector config.mcp_url must match peer's mcp_url"
    );
}

// ─── Test 5: mcp_client_poll_fetches_survivors_from_peer ─────────────────────

/// Seed survivors on node B; subscribe node A to B; poll stream on A; assert events arrive.
/// This test requires two in-process instances using distinct workspaces on the same DB.
/// Spawn order: A first (truncates DB), then B (bootstraps into clean state).
#[tokio::test]
#[ignore]
async fn mcp_client_poll_fetches_survivors_from_peer() {
    // Node A truncates + bootstraps first.
    let (base_a, pool_a) = spawn_app().await;

    // Node B bootstraps into the already-clean DB (no truncate).
    let (base_b, _pool_b) = spawn_second_app().await;

    let org_id = default_org_id(&pool_a).await;

    // Create a separate workspace for node B's data.
    // We use pool_a since both nodes share the same DB after A's truncate+bootstrap.
    let ws_b = insert_workspace(&pool_a, org_id, "Node B Workspace").await;

    // Seed a survivor in workspace B.
    let sig_b = insert_signal(&pool_a, ws_b, "B Signal", "routine").await;
    insert_survivor(&pool_a, sig_b).await;

    // Node A: workspace A.
    let ws_a = default_workspace_id(&pool_a).await;

    // Create a trust issuer on A's pool.
    let (issuer_id, _secret) =
        insert_trust_issuer(&pool_a, org_id, "https://iss-poll.test", "aud-poll").await;

    // Create a peer pointing at node B's MCP endpoint.
    let peer_mcp_url = format!("{}/mcp", base_b);
    let peer_id = insert_peer(&pool_a, "Node B Peer", &peer_mcp_url, issuer_id).await;

    // Subscribe workspace A to the peer — creates mcp connector + first poll in background.
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/workspaces/{}/peers/{}/subscribe",
            base_a, ws_a, peer_id
        ))
        .json(&json!({}))
        .send()
        .await
        .expect("subscribe failed");

    assert_eq!(resp.status(), StatusCode::OK, "subscribe must return 200");

    let connector_body: Value = resp.json().await.expect("response not JSON");
    let connector_id_str = connector_body["id"].as_str().expect("connector id missing");
    let connector_id = Uuid::parse_str(connector_id_str).expect("connector id invalid");

    // Wait for the background first-poll to complete.
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    // If background poll hasn't produced events yet, poll each stream explicitly.
    let streams: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM streams WHERE connector_id = $1")
            .bind(connector_id)
            .fetch_all(&pool_a)
            .await
            .expect("stream list query");

    for (stream_id, _) in &streams {
        let resp = reqwest::Client::new()
            .post(format!("{}/api/v1/streams/{}/poll", base_a, stream_id))
            .json(&json!({}))
            .send()
            .await;
        let _ = resp; // ignore errors — best effort
    }

    // Brief additional wait.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // The key assertion: node A has stream_events from node B's MCP endpoint.
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stream_events se
         JOIN streams s ON s.id = se.stream_id
         WHERE s.connector_id = $1",
    )
    .bind(connector_id)
    .fetch_one(&pool_a)
    .await
    .expect("event count query failed");

    assert!(
        event_count >= 1,
        "node A must have at least 1 stream_event from node B after poll, got {}",
        event_count
    );
}

// ─── Test 6: peer_routing_invokes_propose_artifact_on_peer ───────────────────

/// process_routing_decision with kind=peer invokes propose_artifact on the peer.
/// Spawn order: A truncates first, then B bootstraps into the clean DB.
#[tokio::test]
#[ignore]
async fn peer_routing_invokes_propose_artifact_on_peer() {
    // Node A truncates + bootstraps first.
    let (_, pool_a) = spawn_app().await;

    // Node B bootstraps into the clean DB (no truncate).
    let (base_b, _pool_b) = spawn_second_app().await;

    let org_id = default_org_id(&pool_a).await;
    let ws_a = default_workspace_id(&pool_a).await;

    // Trust issuer for peer auth.
    let (issuer_id, _) =
        insert_trust_issuer(&pool_a, org_id, "https://iss-peer6.test", "aud6").await;

    let peer_mcp_url = format!("{}/mcp", base_b);
    let peer_id = insert_peer(&pool_a, "B Peer 6", &peer_mcp_url, issuer_id).await;

    // Create the mcp connector directly in the DB pointing at node B.
    sqlx::query(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:B Peer 6',
                 jsonb_build_object('mcp_url', $2, 'bearer_token', ''))",
    )
    .bind(ws_a)
    .bind(&peer_mcp_url)
    .execute(&pool_a)
    .await
    .expect("insert mcp connector failed");

    // Seed signal + survivor on A, with peer routing.
    let sig = insert_signal(&pool_a, ws_a, "Peer Routing Signal", "routine").await;
    let survivor_id = insert_survivor(&pool_a, sig).await;

    let routing_id =
        insert_routing_decision(&pool_a, survivor_id, "peer", json!({ "peer_id": peer_id })).await;

    // Count all artifacts before delivery (global, since node B may bootstrap a different workspace).
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count before");

    // Build AppState for A to call process_routing_decision.
    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool_a.clone(), Uuid::nil(), ws_a);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision failed");

    // Artifact count must have increased (artifact was proposed to peer via MCP).
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count after");

    assert!(
        after > before,
        "a new artifact must exist after peer delivery (before={}, after={})",
        before,
        after
    );

    // Audit must record peer_delivered on ws_a.
    let delivered: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events WHERE workspace_id = $1 AND verb = 'peer_delivered' LIMIT 1",
    )
    .bind(ws_a)
    .fetch_optional(&pool_a)
    .await
    .expect("audit query");

    assert_eq!(
        delivered.as_deref(),
        Some("peer_delivered"),
        "must record 'peer_delivered' audit on ws_a"
    );
}

// ─── Test 7: peer_unreachable_records_audit ───────────────────────────────────

/// When there's no mcp connector for a peer in the workspace, delivery writes
/// audit verb='peer_delivery_failed'.
#[tokio::test]
#[ignore]
async fn peer_unreachable_records_audit() {
    let (_, pool) = spawn_app().await;

    let org_id = default_org_id(&pool).await;
    let ws = default_workspace_id(&pool).await;

    let (issuer_id, _) =
        insert_trust_issuer(&pool, org_id, "https://iss-unreach.test", "aud7").await;

    // Peer with a dead URL — and no connector in the workspace.
    let peer_id = insert_peer(&pool, "Dead Peer", "http://127.0.0.1:1/mcp", issuer_id).await;

    let sig = insert_signal(&pool, ws, "Unreachable Signal", "routine").await;
    let survivor_id = insert_survivor(&pool, sig).await;
    let routing_id =
        insert_routing_decision(&pool, survivor_id, "peer", json!({ "peer_id": peer_id })).await;

    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool.clone(), Uuid::nil(), ws);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision must not error out (audit-only)");

    // Check audit verb='peer_delivery_failed'.
    let verb: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events
         WHERE workspace_id = $1 AND verb = 'peer_delivery_failed'
         LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(&pool)
    .await
    .expect("audit query failed");

    assert_eq!(
        verb.as_deref(),
        Some("peer_delivery_failed"),
        "must record audit verb='peer_delivery_failed' when no connector found"
    );
}

// ─── Test 8: sharing_policy_blocks_disallowed_severity ───────────────────────

/// peer.sharing_policy={"allow_severity":["routine"]}; routing with a 'command'
/// severity survivor → audit verb='peer_policy_blocked', no request sent to peer.
#[tokio::test]
#[ignore]
async fn sharing_policy_blocks_disallowed_severity() {
    let (_, pool) = spawn_app().await;

    let org_id = default_org_id(&pool).await;
    let ws = default_workspace_id(&pool).await;

    let (issuer_id, _) = insert_trust_issuer(&pool, org_id, "https://iss-block.test", "aud8").await;

    // Create peer with sharing_policy that only allows 'routine'.
    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy)
         VALUES ('Block Peer', 'http://127.0.0.1:2/mcp', $1,
                 '{\"allow_severity\":[\"routine\"]}'::jsonb)
         RETURNING id",
    )
    .bind(issuer_id)
    .fetch_one(&pool)
    .await
    .expect("insert peer failed");

    // Create mcp connector for this peer in the workspace.
    sqlx::query(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:Block Peer',
                 jsonb_build_object('mcp_url', 'http://127.0.0.1:2/mcp', 'bearer_token', ''))",
    )
    .bind(ws)
    .execute(&pool)
    .await
    .expect("insert connector failed");

    // Signal with severity='command' — should be blocked.
    let sig = insert_signal(&pool, ws, "Command Signal", "command").await;
    let survivor_id = insert_survivor(&pool, sig).await;
    let routing_id =
        insert_routing_decision(&pool, survivor_id, "peer", json!({ "peer_id": peer_id })).await;

    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool.clone(), Uuid::nil(), ws);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision must not error");

    let verb: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events
         WHERE workspace_id = $1 AND verb = 'peer_policy_blocked'
         LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(&pool)
    .await
    .expect("audit query");

    assert_eq!(
        verb.as_deref(),
        Some("peer_policy_blocked"),
        "must record 'peer_policy_blocked' when severity not in allow_severity"
    );

    // No 'peer_delivered' audit row must exist.
    let delivered: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events WHERE workspace_id = $1 AND verb = 'peer_delivered' LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(&pool)
    .await
    .expect("delivered query");

    assert!(
        delivered.is_none(),
        "peer_delivered must NOT be written when policy blocks"
    );
}

// ─── Test 9: sharing_policy_allows_matched_severity ──────────────────────────

/// allow_severity=["flagged","command"]; flagged survivor → delivered (or delivery_failed
/// because the peer URL is dead, but NOT policy_blocked).
#[tokio::test]
#[ignore]
async fn sharing_policy_allows_matched_severity() {
    let (_, pool) = spawn_app().await;

    let org_id = default_org_id(&pool).await;
    let ws = default_workspace_id(&pool).await;

    let (issuer_id, _) = insert_trust_issuer(&pool, org_id, "https://iss-allow.test", "aud9").await;

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (name, mcp_url, issuer_id, sharing_policy)
         VALUES ('Allow Peer', 'http://127.0.0.1:3/mcp', $1,
                 '{\"allow_severity\":[\"flagged\",\"command\"]}'::jsonb)
         RETURNING id",
    )
    .bind(issuer_id)
    .fetch_one(&pool)
    .await
    .expect("insert peer failed");

    // Connector in workspace pointing at dead URL (will fail delivery but not policy).
    sqlx::query(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:Allow Peer',
                 jsonb_build_object('mcp_url', 'http://127.0.0.1:3/mcp', 'bearer_token', ''))",
    )
    .bind(ws)
    .execute(&pool)
    .await
    .expect("insert connector failed");

    // Signal with severity='flagged' — should pass policy, then fail delivery (dead URL).
    let sig = insert_signal(&pool, ws, "Flagged Signal", "flagged").await;
    let survivor_id = insert_survivor(&pool, sig).await;
    let routing_id =
        insert_routing_decision(&pool, survivor_id, "peer", json!({ "peer_id": peer_id })).await;

    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool.clone(), Uuid::nil(), ws);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("process_routing_decision must not error");

    // Must NOT be policy_blocked.
    let blocked: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events WHERE workspace_id = $1 AND verb = 'peer_policy_blocked' LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(&pool)
    .await
    .expect("blocked query");

    assert!(
        blocked.is_none(),
        "peer_policy_blocked must NOT be written when severity is in allow_severity"
    );

    // Must be either peer_delivered or peer_delivery_failed (policy passed, network failed).
    let terminal: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events
         WHERE workspace_id = $1
           AND verb IN ('peer_delivered', 'peer_delivery_failed')
         LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(&pool)
    .await
    .expect("terminal query");

    assert!(
        terminal.is_some(),
        "must have peer_delivered or peer_delivery_failed (policy passed), got none"
    );
}

// ─── Test 10: two_node_federation_end_to_end ─────────────────────────────────

/// Full end-to-end: node A seeds raw events, routing policy targets peer=B,
/// delivery calls propose_artifact on B. Ollama-gated (IONE_SKIP_LIVE=1 skips).
#[tokio::test]
#[ignore]
async fn two_node_federation_end_to_end() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("two_node_federation_end_to_end: skipped (IONE_SKIP_LIVE)");
        return;
    }

    // Node A truncates + bootstraps first.
    let (_, pool_a) = spawn_app().await;

    // Node B bootstraps into the clean DB.
    let (base_b, pool_b) = spawn_second_app().await;

    let org_id = default_org_id(&pool_a).await;
    let ws_a = default_workspace_id(&pool_a).await;
    let _ = &pool_b; // pool_b kept alive to keep node B running

    let (issuer_id, _) =
        insert_trust_issuer(&pool_a, org_id, "https://iss-e2e.test", "aud-e2e").await;

    let peer_mcp_url = format!("{}/mcp", base_b);
    let peer_id = insert_peer(&pool_a, "E2E Peer", &peer_mcp_url, issuer_id).await;

    // Create mcp connector for peer in workspace A.
    sqlx::query(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'peer:E2E Peer',
                 jsonb_build_object('mcp_url', $2, 'bearer_token', ''))",
    )
    .bind(ws_a)
    .bind(&peer_mcp_url)
    .execute(&pool_a)
    .await
    .expect("insert mcp connector for e2e");

    // Seed raw events on A (simulate scheduler tick output).
    let sig = insert_signal(&pool_a, ws_a, "E2E Signal", "flagged").await;
    let survivor_id = insert_survivor(&pool_a, sig).await;

    let routing_id =
        insert_routing_decision(&pool_a, survivor_id, "peer", json!({ "peer_id": peer_id })).await;

    // Use global artifact count to avoid workspace_id mismatch from dual bootstrap.
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count before");

    let config = ione::config::Config::from_env();
    let state = ione::state::AppState::new(config, pool_a.clone(), Uuid::nil(), ws_a);

    ione::services::delivery::process_routing_decision(&state, routing_id)
        .await
        .expect("e2e process_routing_decision");

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts")
        .fetch_one(&pool_a)
        .await
        .expect("count after");

    assert!(
        after > before,
        "a new artifact must exist after e2e peer delivery (before={}, after={})",
        before,
        after
    );

    // Audit 'peer_delivered' on A.
    let verb: Option<String> = sqlx::query_scalar(
        "SELECT verb FROM audit_events WHERE workspace_id = $1 AND verb = 'peer_delivered' LIMIT 1",
    )
    .bind(ws_a)
    .fetch_optional(&pool_a)
    .await
    .expect("audit delivered query");

    assert_eq!(
        verb.as_deref(),
        Some("peer_delivered"),
        "must record 'peer_delivered' on successful e2e delivery"
    );
}

// ─── Test 11: unknown_peer_issuer_is_rejected_on_mcp_inbound ─────────────────

/// Phase 11 regression: bearer JWT from an issuer not in trust_issuers must still
/// be 401 at /mcp (in oidc mode). In local mode the fallback masks this.
#[tokio::test]
#[ignore]
async fn unknown_peer_issuer_is_rejected_on_mcp_inbound() {
    let auth_mode = std::env::var("IONE_AUTH_MODE").unwrap_or_default();
    if auth_mode.to_lowercase() != "oidc" {
        eprintln!(
            "unknown_peer_issuer_is_rejected_on_mcp_inbound: skipping (IONE_AUTH_MODE != oidc)"
        );
        return;
    }

    let (base, _pool) = spawn_app().await;

    // Sign with a random secret not registered in trust_issuers.
    let rogue_secret: Vec<u8> = (200u8..232).collect();
    let token = mint_jwt(
        "rogue-sub",
        "https://rogue.issuer.test",
        "ione-mcp",
        &rogue_secret,
    );

    let resp = reqwest::Client::new()
        .post(format!("{}/mcp", base))
        .header("Authorization", format!("Bearer {}", token))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_workspaces", "arguments": {} }
        }))
        .send()
        .await
        .expect("POST /mcp failed");

    assert_eq!(resp.status(), StatusCode::OK, "MCP returns errors in body");

    let body: Value = resp.json().await.expect("response not JSON");
    assert!(
        !body["error"].is_null(),
        "untrusted bearer must produce a JSON-RPC error, got: {}",
        body
    );
    let code = body["error"]["code"].as_i64().unwrap_or(0);
    assert_eq!(
        code, -32001,
        "untrusted bearer must return error code -32001, got: {}",
        code
    );
}

// ─── Test 12: peer_list_returns_items ────────────────────────────────────────

/// GET /api/v1/peers returns { items: [...] } after inserting a peer.
#[tokio::test]
#[ignore]
async fn peer_list_returns_items() {
    let (base, pool) = spawn_app().await;

    let org_id = default_org_id(&pool).await;
    let (issuer_id, _) =
        insert_trust_issuer(&pool, org_id, "https://iss-list.test", "aud-list").await;

    insert_peer(
        &pool,
        "List Peer",
        "https://list.peer.example.com/mcp",
        issuer_id,
    )
    .await;

    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/peers", base))
        .send()
        .await
        .expect("GET /api/v1/peers failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "GET /api/v1/peers must return 200"
    );

    let body: Value = resp.json().await.expect("response not JSON");
    let items = body["items"].as_array().expect("items must be an array");

    assert!(
        !items.is_empty(),
        "items must not be empty after inserting a peer"
    );

    let peer = items
        .iter()
        .find(|p| p["mcpUrl"].as_str() == Some("https://list.peer.example.com/mcp"));
    assert!(
        peer.is_some(),
        "list must include the seeded peer, got: {:?}",
        items
    );

    let p = peer.unwrap();
    assert!(!p["id"].is_null(), "peer must have id");
    assert!(!p["name"].is_null(), "peer must have name");
    assert!(!p["status"].is_null(), "peer must have status");
    assert!(!p["createdAt"].is_null(), "peer must have createdAt");
}
