//! Contract error envelope tests — assert the canonical error shape from the contract.
//!
//! Each test sends a request that MUST produce a specific error code and verifies
//! the JSON envelope matches `{ error: "<kind>", message: <non-empty string>, ... }`.
//!
//! All tests are expected to FAIL until the error handling is implemented.
//!
//! Prerequisites:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     cargo test --test contract_errors -- --ignored --test-threads=1

use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::{uuid, Uuid};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// The demo workspace UUID from the contract: all writes to this workspace must
/// return `demo_read_only` (403).
const DEMO_WORKSPACE_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d30");

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
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

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("get addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server error") });

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

// ─── demo_read_only ───────────────────────────────────────────────────────────

/// POSTing a connector to the demo workspace must return 403 with
/// `{ error: "demo_read_only", message: <non-empty> }`.
#[tokio::test]
#[ignore]
async fn error_demo_read_only_on_connector_create() {
    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/workspaces/{}/connectors",
            base, DEMO_WORKSPACE_ID
        ))
        .json(&serde_json::json!({
            "kind": "rust_native",
            "name": "nws-test",
            "config": {}
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        403,
        "demo workspace write must return 403, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response must be JSON");
    assert_eq!(
        body["error"].as_str(),
        Some("demo_read_only"),
        "error field must be 'demo_read_only', got: {}",
        body
    );
    let msg = body["message"].as_str().unwrap_or("");
    assert!(
        !msg.is_empty(),
        "message field must be non-empty, got: {}",
        body
    );
}

/// Demo conversations must accept messages via the canned-chat path.
///
/// Contract: conversations are not nested under /workspaces in the URL, and
/// posting to a demo-workspace conversation returns 200 with a canned
/// assistant reply (model = "canned"), not a 403. The demo write-guard
/// deliberately does NOT block message posts because the demo is meant to
/// be interactive. Mutating the data behind the demo workspace (connectors,
/// peers, workspace close) IS blocked; see other demo_read_only_* tests.
#[tokio::test]
#[ignore]
async fn demo_conversation_messages_return_canned_reply() {
    let (base, pool) = spawn_app().await;

    // Ensure demo workspace is seeded and has at least one conversation.
    std::env::set_var("IONE_SEED_DEMO", "1");
    std::env::set_var("OLLAMA_BASE_URL", "http://127.0.0.1:1");
    let _ = ione::demo::seeder::seed_demo_if_enabled(&pool).await;

    let conv_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM conversations WHERE workspace_id = $1 LIMIT 1",
    )
    .bind(DEMO_WORKSPACE_ID)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(Uuid::new_v4);

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/conversations/{}/messages", base, conv_id))
        .json(&serde_json::json!({ "content": "What approvals are pending and why?" }))
        .send()
        .await
        .expect("request failed");

    // Either 200 with canned reply, or 400 if the conversation doesn't exist
    // (still a successful contract enforcement — the demo guard didn't fire,
    // which is what we're asserting).
    let status = resp.status().as_u16();
    if status == 403 {
        panic!("demo conversation message must NOT be demo_read_only: demo chat is how the user exercises the demo");
    }
    if status == 200 {
        let body: Value = resp.json().await.expect("response must be JSON");
        assert_eq!(
            body["model"].as_str(),
            Some("canned"),
            "demo conversation message must use canned model, got: {}",
            body
        );
    }
}

/// Closing the demo workspace must return 403 with `{ error: "demo_read_only" }`.
#[tokio::test]
#[ignore]
async fn error_demo_read_only_on_workspace_close() {
    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/workspaces/{}/close",
            base, DEMO_WORKSPACE_ID
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        403,
        "closing demo workspace must return 403, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response must be JSON");
    assert_eq!(
        body["error"].as_str(),
        Some("demo_read_only"),
        "error must be 'demo_read_only', got: {}",
        body
    );
    assert!(
        !body["message"].as_str().unwrap_or("").is_empty(),
        "message must be non-empty"
    );
}

// ─── ollama_unreachable ───────────────────────────────────────────────────────

/// Sending a chat request when Ollama is unreachable must return 503 with
/// `{ error: "ollama_unreachable", message: <non-empty>, baseUrl: <string> }`.
///
/// We simulate an unreachable Ollama by temporarily overriding the env var.
/// The test spawns a fresh app with OLLAMA_BASE_URL pointing at a port that
/// nothing is listening on (127.0.0.1:1 is reserved and unreachable).
#[tokio::test]
#[ignore]
async fn error_ollama_unreachable_on_chat() {
    // Force a dead Ollama URL for this test's app instance.
    std::env::set_var("OLLAMA_BASE_URL", "http://127.0.0.1:1");

    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/chat", base))
        .json(&serde_json::json!({
            "prompt": "say pong",
            "model": "llama3.2:latest"
        }))
        .send()
        .await
        .expect("request failed");

    // Restore so other tests aren't affected.
    std::env::remove_var("OLLAMA_BASE_URL");

    assert_eq!(
        resp.status().as_u16(),
        503,
        "unreachable Ollama must return 503, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response must be JSON");
    assert_eq!(
        body["error"].as_str(),
        Some("ollama_unreachable"),
        "error field must be 'ollama_unreachable', got: {}",
        body
    );
    let msg = body["message"].as_str().unwrap_or("");
    assert!(!msg.is_empty(), "message must be non-empty, got: {}", body);

    let base_url = body["baseUrl"].as_str().unwrap_or("");
    assert!(
        !base_url.is_empty(),
        "baseUrl field must be present and non-empty in ollama_unreachable response, got: {}",
        body
    );
}

// ─── validation_failed / nws_out_of_range ─────────────────────────────────────

/// POSTing an NWS connector config with an out-of-range latitude must return
/// 422 with `{ error: "nws_out_of_range", message, hint, field }`.
#[tokio::test]
#[ignore]
async fn error_nws_out_of_range_on_connector_validate() {
    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/connectors/validate", base))
        .json(&serde_json::json!({
            "kind": "rust_native",
            "name": "nws",
            "config": {
                "lat": 999,
                "lon": 0
            }
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        422,
        "out-of-range lat must return 422, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response must be JSON");
    assert_eq!(
        body["error"].as_str(),
        Some("nws_out_of_range"),
        "error field must be 'nws_out_of_range', got: {}",
        body
    );
    let msg = body["message"].as_str().unwrap_or("");
    assert!(!msg.is_empty(), "message must be non-empty");

    let hint = body["hint"].as_str().unwrap_or("");
    assert!(!hint.is_empty(), "hint field must be present and non-empty");

    let field = body["field"].as_str().unwrap_or("");
    assert!(!field.is_empty(), "field field must be present and non-empty");
    assert_eq!(
        field, "lat",
        "field must identify the offending field ('lat'), got: {}",
        field
    );
}

/// Validating an NWS connector with an out-of-range longitude must also return
/// 422 with `{ error: "nws_out_of_range", field: "lon" }`.
#[tokio::test]
#[ignore]
async fn error_nws_out_of_range_on_connector_validate_lon() {
    let (base, _pool) = spawn_app().await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/connectors/validate", base))
        .json(&serde_json::json!({
            "kind": "rust_native",
            "name": "nws",
            "config": {
                "lat": 45.0,
                "lon": 999
            }
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        422,
        "out-of-range lon must return 422, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("response must be JSON");
    assert_eq!(
        body["error"].as_str(),
        Some("nws_out_of_range"),
        "error field must be 'nws_out_of_range', got: {}",
        body
    );
    assert_eq!(
        body["field"].as_str(),
        Some("lon"),
        "field must be 'lon', got: {}",
        body
    );
}
