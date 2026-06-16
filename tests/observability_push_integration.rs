//! Integration tests for MCP SSE interaction notifications.
//!
//! Run: DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!   cargo test --test observability_push_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::Utc;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "observability-push-test-bearer";

async fn spawn_app_with_state() -> (String, PgPool, ione::state::AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrate");
    sqlx::query(
        "TRUNCATE interaction_events, webhook_events_seen, workspace_peer_bindings, audit_events,
                  pipeline_events, approvals, artifacts, trust_issuers, peers,
                  routing_decisions, survivors, signals, stream_events, streams, connectors,
                  service_account_tokens, org_memberships, memberships, roles, messages,
                  conversations, workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate");

    let (app, state) = ione::app_with_state(pool.clone()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool, state)
}

async fn default_ids(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let org_id: Uuid = sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org");
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("user");
    let workspace_id: Uuid = sqlx::query_scalar("SELECT id FROM workspaces LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("workspace");
    (org_id, user_id, workspace_id)
}

async fn elevate_default_user_to_audit_read(pool: &PgPool, workspace_id: Uuid) {
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("user");
    let role_id: Uuid = sqlx::query_scalar(
        "INSERT INTO roles (workspace_id, name, coc_level, permissions)
         VALUES ($1, $2, 50, '[\"audit:read\"]'::jsonb)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("audit-read-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("role");
    sqlx::query(
        "INSERT INTO memberships (user_id, workspace_id, role_id)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(workspace_id)
    .bind(role_id)
    .execute(pool)
    .await
    .expect("membership");
}

async fn insert_peer(pool: &PgPool, org_id: Uuid, prefix: &str) -> Uuid {
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', $3, '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://issuer.example/{}", Uuid::new_v4()))
    .bind(format!("https://issuer.example/{}/jwks", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("issuer");
    sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, status, tool_prefix)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, 'active'::peer_status, $5)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("{prefix}-{}", Uuid::new_v4()))
    .bind(format!("https://peer.example/mcp/{}", Uuid::new_v4()))
    .bind(issuer_id)
    .bind(prefix)
    .fetch_one(pool)
    .await
    .expect("peer")
}

fn event(
    org_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    user_id: Uuid,
) -> ione::models::InteractionEvent {
    ione::models::InteractionEvent {
        id: Uuid::new_v4(),
        org_id,
        workspace_id,
        peer_id,
        peer_name: "push-peer".into(),
        tool_name: "lookup".into(),
        caller_kind: ione::models::ActorKind::User,
        caller_user_id: Some(user_id),
        caller_peer_id: None,
        caller_token_id: None,
        session_id: Some(Uuid::new_v4()),
        sequence_number: Some(1),
        outcome: "allow".into(),
        latency_ms: Some(3),
        detail: json!({}),
        recorded_at: Utc::now(),
    }
}

async fn read_until_contains(resp: reqwest::Response, needle: &str) -> bool {
    let mut stream = resp.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(2));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            chunk = stream.next() => {
                let Some(chunk) = chunk else { return false; };
                let chunk = chunk.expect("sse chunk");
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains(needle) {
                    return true;
                }
            }
            _ = &mut deadline => return false,
        }
    }
}

#[tokio::test]
#[ignore]
async fn workspace_sse_receives_matching_interaction_notification() {
    let (base, pool, state) = spawn_app_with_state().await;
    let (org_id, user_id, workspace_id) = default_ids(&pool).await;
    elevate_default_user_to_audit_read(&pool, workspace_id).await;
    let peer_id = insert_peer(&pool, org_id, "push").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/mcp?workspace_id={workspace_id}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("sse request");
    assert_eq!(resp.status(), StatusCode::OK);

    let ev = event(org_id, workspace_id, peer_id, user_id);
    let event_id = ev.id.to_string();
    state.interaction_sink.emit(ev);
    assert!(read_until_contains(resp, &event_id).await);
}

#[tokio::test]
#[ignore]
async fn cross_org_subscription_is_hidden_and_other_workspace_events_do_not_leak() {
    let (base, pool, state) = spawn_app_with_state().await;
    let (org_id, user_id, workspace_a) = default_ids(&pool).await;
    elevate_default_user_to_audit_read(&pool, workspace_a).await;
    let workspace_b: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("workspace-b-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("workspace b");
    elevate_default_user_to_audit_read(&pool, workspace_b).await;
    let peer_id = insert_peer(&pool, org_id, "pushb").await;

    let foreign_org: Uuid =
        sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id")
            .bind(format!("foreign-{}", Uuid::new_v4()))
            .fetch_one(&pool)
            .await
            .expect("foreign org");
    let foreign_workspace: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(foreign_org)
    .bind(format!("foreign-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("foreign workspace");

    let client = reqwest::Client::new();
    let hidden = client
        .get(format!("{base}/mcp?workspace_id={foreign_workspace}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("foreign sse");
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);

    let resp = client
        .get(format!("{base}/mcp?workspace_id={workspace_b}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("workspace b sse");
    assert_eq!(resp.status(), StatusCode::OK);
    let workspace_a_event = event(org_id, workspace_a, peer_id, user_id);
    let event_id = workspace_a_event.id.to_string();
    state.interaction_sink.emit(workspace_a_event);
    assert!(!read_until_contains(resp, &event_id).await);
}
