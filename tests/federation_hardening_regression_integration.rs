//! Negative-path regression tests for the tri-thrust federation-hardening fixes
//! (branch `feat/tri-thrust-federation-hardening`). Each test pins a guard that
//! a prior session shipped but did not cover:
//!
//!   TT-A02  subscribe rejects a non-active peer
//!   TT-A04  binding mutations require `peers:manage` (create + delete)
//!   TT-A07  subscribe defers the first poll when no Active binding exists
//!   TT-C01  oversized stream-event payloads are Rejected, not stored
//!   TT-C09  oversized webhook `data` is rejected by envelope validation
//!
//! Run (serial, ignored, live DB):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     cargo test --test federation_hardening_regression_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

use ione::repos::{InsertOutcome, StreamEventRepo};

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

// ─── seed helpers ─────────────────────────────────────────────────────────────

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations ORDER BY created_at LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org not found")
}

async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

/// Set the default user's bootstrap `member` role permissions in place.
async fn set_member_permissions(pool: &PgPool, workspace_id: Uuid, perms: Value) {
    sqlx::query("UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'")
        .bind(workspace_id)
        .bind(perms)
        .execute(pool)
        .await
        .expect("set member permissions");
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

async fn insert_active_binding(
    pool: &PgPool,
    org_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace_peer_bindings (org_id, workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, $3, 't-acme', 'active'::binding_status)
         RETURNING id",
    )
    .bind(org_id)
    .bind(workspace_id)
    .bind(peer_id)
    .fetch_one(pool)
    .await
    .expect("binding")
}

/// Seed a connector + stream under `workspace_id`, returning the stream id.
async fn insert_stream(pool: &PgPool, workspace_id: Uuid) -> Uuid {
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'mcp'::connector_kind, 'test-connector', '{}'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("connector");
    sqlx::query_scalar(
        "INSERT INTO streams (connector_id, name) VALUES ($1, 'test-stream') RETURNING id",
    )
    .bind(connector_id)
    .fetch_one(pool)
    .await
    .expect("stream")
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

fn envelope(peer_id: Uuid, event_id: &str, data: Value) -> Value {
    json!({
        "id": event_id,
        "type": "alert.created",
        "occurred_at": Utc::now(),
        "peer_id": peer_id,
        "foreign_tenant_id": "t-acme",
        "severity": "routine",
        "data": data,
        "approval_required": false
    })
}

// ─── TT-A04: binding mutations require peers:manage ───────────────────────────

/// POST a binding without `peers:manage` is forbidden; granting it clears the gate.
#[tokio::test]
#[ignore]
async fn create_binding_403_without_peers_manage() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let url = format!("{base}/api/v1/workspaces/{ws}/bindings");
    let req = json!({ "peerId": peer_id, "foreignTenantId": "t-acme", "scope": {} });

    set_member_permissions(&pool, ws, json!(["peers:read"])).await;
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("create binding");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // With the grant, the request passes the RBAC gate (no longer 403).
    set_member_permissions(&pool, ws, json!(["admin"])).await;
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("create binding (granted)");
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}

/// DELETE on an existing binding without `peers:manage` is forbidden.
#[tokio::test]
#[ignore]
async fn delete_binding_403_without_peers_manage() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let binding_id = insert_active_binding(&pool, org_id, ws, peer_id).await;
    set_member_permissions(&pool, ws, json!(["peers:read"])).await;

    let resp = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/workspaces/{ws}/bindings/{binding_id}"
        ))
        .send()
        .await
        .expect("delete binding");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // The binding is untouched by the rejected delete.
    let still_there: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_peer_bindings WHERE id = $1")
            .bind(binding_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(still_there, 1);
}

// ─── TT-A02 / TT-A07: subscribe peer-status guard + deferred first poll ───────

/// Subscribing a non-active peer is a 400; the message names the status.
#[tokio::test]
#[ignore]
async fn subscribe_rejects_non_active_peer() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "paused").await;
    set_member_permissions(&pool, ws, json!(["admin"])).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{ws}/peers/{peer_id}/subscribe"
        ))
        .send()
        .await
        .expect("subscribe");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.text().await.expect("body");
    assert!(
        body.contains("not active"),
        "expected a not-active rejection, got: {body}"
    );
}

/// Subscribing an active peer with no Active binding succeeds but defers the
/// first poll (`firstPollDeferred: true`) rather than polling unscoped.
#[tokio::test]
#[ignore]
async fn subscribe_active_peer_defers_first_poll() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    set_member_permissions(&pool, ws, json!(["admin"])).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/workspaces/{ws}/peers/{peer_id}/subscribe"
        ))
        .send()
        .await
        .expect("subscribe");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["firstPollDeferred"],
        json!(true),
        "no Active binding exists, so the first poll must be deferred: {body}"
    );
}

// ─── TT-C01: oversized stream-event payloads are Rejected ─────────────────────

#[tokio::test]
#[ignore]
async fn oversized_stream_event_is_rejected() {
    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let stream_id = insert_stream(&pool, ws).await;
    let repo = StreamEventRepo::new(pool.clone());

    // > 65_536 serialized bytes: rejected, not stored.
    let big = json!({ "blob": "x".repeat(70_000) });
    let outcome = repo
        .insert_event(stream_id, big, Utc::now(), None)
        .await
        .expect("insert big");
    assert_eq!(outcome, InsertOutcome::Rejected);

    // A small payload still inserts.
    let small = json!({ "blob": "hi" });
    let outcome = repo
        .insert_event(stream_id, small, Utc::now(), None)
        .await
        .expect("insert small");
    assert_eq!(outcome, InsertOutcome::Inserted);

    // Exactly one row landed (the rejected one never hit the table).
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stream_events WHERE stream_id = $1")
        .bind(stream_id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 1);
}

// ─── TT-C09: oversized webhook `data` is rejected by envelope validation ──────

#[tokio::test]
#[ignore]
async fn oversized_webhook_data_is_rejected() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;
    insert_active_binding(&pool, org_id, ws, peer_id).await;
    let client = reqwest::Client::new();

    // A normal-sized event with an Active binding is accepted.
    let ok_body = serde_json::to_string(&envelope(peer_id, "evt-ok", json!({ "message": "hi" })))
        .expect("body");
    let resp = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &ok_body, Utc::now().timestamp()),
        )
        .body(ok_body)
        .send()
        .await
        .expect("post ok");
    assert_eq!(resp.status(), StatusCode::OK);

    // Same setup, but `data` exceeds the 100 KiB cap: rejected at validation.
    let big_data = json!({ "blob": "x".repeat(110_000) });
    let big_body = serde_json::to_string(&envelope(peer_id, "evt-big", big_data)).expect("body");
    let resp = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &big_body, Utc::now().timestamp()),
        )
        .body(big_body)
        .send()
        .await
        .expect("post big");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
