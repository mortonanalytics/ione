//! Integration tests for interaction event read APIs.
//!
//! Run: DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!   cargo test --test observability_read_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use reqwest::StatusCode;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "observability-read-test-bearer";

async fn spawn_app() -> (String, PgPool) {
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

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool)
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

async fn seed_event(
    pool: &PgPool,
    org_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    user_id: Uuid,
    outcome: &str,
    recorded_at: DateTime<Utc>,
    session_id: Option<Uuid>,
    sequence_number: Option<i64>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO interaction_events
           (org_id, workspace_id, peer_id, peer_name, tool_name, caller_kind, caller_user_id,
            session_id, sequence_number, outcome, latency_ms, detail, recorded_at)
         VALUES ($1, $2, $3, 'read-peer', 'lookup', 'user'::actor_kind, $4,
                 $5, $6, $7, CASE WHEN $7 = 'deny' THEN NULL ELSE 8 END, '{}'::jsonb, $8)
         RETURNING id",
    )
    .bind(org_id)
    .bind(workspace_id)
    .bind(peer_id)
    .bind(user_id)
    .bind(session_id)
    .bind(sequence_number)
    .bind(outcome)
    .bind(recorded_at)
    .fetch_one(pool)
    .await
    .expect("seed interaction")
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn get(base: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(TEST_STATIC_BEARER)
        .send()
        .await
        .expect("request")
}

#[tokio::test]
#[ignore]
async fn list_filters_cursor_limit_and_session_replay_work() {
    let (base, pool) = spawn_app().await;
    let (org_id, user_id, workspace_id) = default_ids(&pool).await;
    elevate_default_user_to_audit_read(&pool, workspace_id).await;
    let peer_id = insert_peer(&pool, org_id, "read").await;
    let t0 = Utc::now() - Duration::hours(2);
    let session_id = Uuid::new_v4();
    for i in 0..5 {
        seed_event(
            &pool,
            org_id,
            workspace_id,
            peer_id,
            user_id,
            if i == 1 { "deny" } else { "allow" },
            t0 + Duration::minutes(i),
            Some(session_id),
            Some(i + 1),
        )
        .await;
    }

    let resp = get(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/interaction-events?peer_id={peer_id}&since={}&limit=2",
            ts(t0)
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert!(body["next_cursor"].as_str().is_some());

    let session = get(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/interaction-sessions/{session_id}"),
    )
    .await
    .json::<Value>()
    .await
    .expect("session json");
    let seqs: Vec<i64> = session["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["sequenceNumber"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
#[ignore]
async fn aggregates_validate_inputs_and_return_counts() {
    let (base, pool) = spawn_app().await;
    let (org_id, user_id, workspace_id) = default_ids(&pool).await;
    elevate_default_user_to_audit_read(&pool, workspace_id).await;
    let peer_id = insert_peer(&pool, org_id, "agg").await;
    let t0 = Utc::now() - Duration::hours(1);
    for i in 0..3 {
        seed_event(
            &pool,
            org_id,
            workspace_id,
            peer_id,
            user_id,
            if i == 0 { "deny" } else { "allow" },
            t0 + Duration::minutes(i),
            None,
            None,
        )
        .await;
    }

    let summary = get(
        &base,
        &format!("/api/v1/workspaces/{workspace_id}/interaction-aggregates?op=outcome_summary"),
    )
    .await
    .json::<Value>()
    .await
    .expect("summary json");
    assert_eq!(summary["op"], "outcome_summary");
    assert_eq!(summary["outcomes"].as_array().unwrap().len(), 2);

    let bucket = get(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/interaction-aggregates?op=count_by_bucket&bucket=hour"
        ),
    )
    .await
    .json::<Value>()
    .await
    .expect("bucket json");
    assert_eq!(bucket["bucket"], "hour");

    let bad = get(
        &base,
        &format!(
            "/api/v1/workspaces/{workspace_id}/interaction-aggregates?op=outcome_summary&bucket=hour"
        ),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn audit_read_required_and_foreign_workspace_hidden() {
    let (base, pool) = spawn_app().await;
    let (org_id, user_id, default_workspace) = default_ids(&pool).await;
    let no_access_workspace: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("no-access-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("workspace");
    let peer_id = insert_peer(&pool, org_id, "deny").await;
    seed_event(
        &pool,
        org_id,
        no_access_workspace,
        peer_id,
        user_id,
        "allow",
        Utc::now(),
        None,
        None,
    )
    .await;

    let forbidden = get(
        &base,
        &format!("/api/v1/workspaces/{no_access_workspace}/interaction-events"),
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

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
    let not_found = get(
        &base,
        &format!("/api/v1/workspaces/{foreign_workspace}/interaction-events"),
    )
    .await;
    assert_eq!(not_found.status(), StatusCode::NOT_FOUND);

    elevate_default_user_to_audit_read(&pool, default_workspace).await;
    let ok = get(
        &base,
        &format!("/api/v1/workspaces/{default_workspace}/interaction-events"),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
}
