use std::net::SocketAddr;

use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
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
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool)
}

async fn spawn_state() -> (PgPool, ione::state::AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

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
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");
    let (_app, state) = ione::app_with_state(pool.clone()).await;
    (pool, state)
}

async fn default_ids(pool: &PgPool) -> (Uuid, Uuid) {
    let org_id = sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org");
    let workspace_id = sqlx::query_scalar("SELECT id FROM workspaces LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("workspace");
    (org_id, workspace_id)
}

async fn insert_peer(pool: &PgPool, org_id: Uuid, status: &str) -> Uuid {
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://issuer-{status}.test"))
    .fetch_one(pool)
    .await
    .expect("issuer");
    sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, status)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, $5::peer_status)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("peer-{status}"))
    .bind(format!("https://peer-{status}.test/mcp"))
    .bind(issuer_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("peer")
}

async fn provision(base: &str, peer_id: Uuid) -> String {
    let body: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/webhook/provision"))
        .send()
        .await
        .expect("provision request")
        .json()
        .await
        .expect("provision json");
    body["signingSecret"]
        .as_str()
        .expect("signingSecret")
        .to_string()
}

fn signed_headers(secret: &str, body: &str, ts: i64) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let digest = hex::encode(mac.finalize().into_bytes());
    format!("t={ts},v1={digest}")
}

fn envelope(peer_id: Uuid, event_id: &str, tenant: &str) -> Value {
    json!({
        "id": event_id,
        "type": "alert.created",
        "occurred_at": Utc::now(),
        "peer_id": peer_id,
        "foreign_tenant_id": tenant,
        "severity": "routine",
        "data": { "message": "hello" },
        "approval_required": false
    })
}

#[tokio::test]
#[ignore]
async fn provision_returns_secret_and_stores_ciphertext() {
    let (base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/webhook/provision"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(body["signingSecret"].as_str().unwrap_or("").len() >= 64);
    assert!(body["webhookUrl"]
        .as_str()
        .unwrap_or("")
        .ends_with(&format!("/webhooks/peer/{peer_id}")));

    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT webhook_secret_ciphertext FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("ciphertext");
    assert!(stored.is_some());
}

#[tokio::test]
#[ignore]
async fn valid_event_replays_and_no_binding_do_not_poison_dedup() {
    let (base, pool) = spawn_app().await;
    let (org_id, workspace_id) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;

    let event = envelope(peer_id, "evt-1", "t-acme");
    let body = serde_json::to_string(&event).expect("body");
    let sig = signed_headers(&secret, &body, Utc::now().timestamp());
    let client = reqwest::Client::new();
    let no_binding = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", &sig)
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(no_binding.status(), StatusCode::BAD_REQUEST);
    let seen: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events_seen WHERE event_id = 'evt-1'")
            .fetch_one(&pool)
            .await
            .expect("seen count");
    assert_eq!(seen, 0);

    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 't-acme', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(&pool)
    .await
    .expect("binding");

    let accepted = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted_body: Value = accepted.json().await.expect("json");
    assert_eq!(accepted_body["duplicate"], false);

    let replay = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    let replay_body: Value = replay.json().await.expect("json");
    assert_eq!(replay_body["duplicate"], true);
}

#[tokio::test]
#[ignore]
async fn stale_invalid_and_revoked_webhooks_are_rejected() {
    let (base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;
    let mut event = envelope(peer_id, "evt-invalid", "t-acme");
    event["occurred_at"] = json!(Utc::now() - Duration::minutes(10));
    let body = serde_json::to_string(&event).expect("body");
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    sqlx::query("UPDATE peers SET status = 'revoked'::peer_status WHERE id = $1")
        .bind(peer_id)
        .execute(&pool)
        .await
        .expect("revoke peer");
    let body = serde_json::to_string(&envelope(peer_id, "evt-revoked", "t-acme")).expect("body");
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn delivery_pass_creates_foreign_tenant_approval_for_gated_signal() {
    let (pool, state) = spawn_state().await;
    let (_org_id, workspace_id) = default_ids(&pool).await;
    let signal_id: Uuid = sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, evidence, severity, approval_required)
         VALUES ($1, 'connector_event'::signal_source, 'Webhook command', 'Body',
                 $2, 'command'::severity, true)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(json!({ "foreign_tenant_id": "t-acme" }))
    .fetch_one(&pool)
    .await
    .expect("signal");
    let survivor_id: Uuid = sqlx::query_scalar(
        "INSERT INTO survivors (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'test', 'survive'::critic_verdict, 'ok', 1.0, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(&pool)
    .await
    .expect("survivor");
    sqlx::query(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, 'draft'::routing_target, '{}'::jsonb, 'test', 'forced')",
    )
    .bind(survivor_id)
    .execute(&pool)
    .await
    .expect("routing decision");

    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("scheduler tick");

    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT ap.foreign_tenant_id
         FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1
         LIMIT 1",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("approval");
    assert_eq!(tenant.as_deref(), Some("t-acme"));
}
