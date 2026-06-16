//! Integration tests for the IONe observability data plane capture path.
//!
//! Run: DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!   cargo test --test observability_capture_integration -- --ignored --test-threads=1

use chrono::Utc;
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn setup_pool() -> PgPool {
    std::env::set_var("IONE_AUTH_MODE", "local");
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
    let _ = ione::app(pool.clone()).await;
    pool
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

async fn insert_peer(pool: &PgPool, org_id: Uuid) -> Uuid {
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
         VALUES ($1, $2, $3, $4, '{}'::jsonb, 'active'::peer_status, 'obs')
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("obs-peer-{}", Uuid::new_v4()))
    .bind(format!("https://peer.example/mcp/{}", Uuid::new_v4()))
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("peer")
}

fn event(
    org_id: Uuid,
    workspace_id: Uuid,
    peer_id: Uuid,
    user_id: Uuid,
    outcome: &str,
    session_id: Option<Uuid>,
    sequence_number: Option<i64>,
) -> ione::models::InteractionEvent {
    ione::models::InteractionEvent {
        id: Uuid::new_v4(),
        org_id,
        workspace_id,
        peer_id,
        peer_name: "obs-peer".into(),
        tool_name: "lookup".into(),
        caller_kind: ione::models::ActorKind::User,
        caller_user_id: Some(user_id),
        caller_peer_id: None,
        caller_token_id: None,
        session_id,
        sequence_number,
        outcome: outcome.into(),
        latency_ms: (outcome != "deny").then_some(12),
        detail: json!({ "code": outcome }),
        recorded_at: Utc::now(),
    }
}

async fn wait_for_count(pool: &PgPool, expected: i64) {
    for _ in 0..50 {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM interaction_events")
            .fetch_one(pool)
            .await
            .expect("count");
        if count >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("interaction_events did not reach {expected}");
}

#[tokio::test]
#[ignore]
async fn capture_writer_persists_outcomes_sequences_and_service_account_principal() {
    let pool = setup_pool().await;
    let (org_id, user_id, workspace_id) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id).await;
    let (sink, rx) = ione::services::interaction_sink::InteractionSink::new();
    let writer = ione::services::interaction_sink::spawn_writer(pool.clone(), rx);

    sink.emit(event(
        org_id,
        workspace_id,
        peer_id,
        user_id,
        "allow",
        None,
        None,
    ));
    sink.emit(event(
        org_id,
        workspace_id,
        peer_id,
        user_id,
        "deny",
        None,
        None,
    ));

    let token_id: Uuid = sqlx::query_scalar(
        "INSERT INTO service_account_tokens (org_id, name, token_hash, permissions)
         VALUES ($1, $2, $3, '[\"tool_invoke:*:*\"]'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("obs-token-{}", Uuid::new_v4()))
    .bind(format!("hash-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("service token");
    let mut service_event = event(org_id, workspace_id, peer_id, user_id, "allow", None, None);
    service_event.caller_kind = ione::models::ActorKind::ServiceAccount;
    service_event.caller_user_id = None;
    service_event.caller_token_id = Some(token_id);
    sink.emit(service_event);

    let session_id = Uuid::new_v4();
    for i in 1..=20 {
        sink.emit(event(
            org_id,
            workspace_id,
            peer_id,
            user_id,
            "allow",
            Some(session_id),
            Some(i),
        ));
    }
    drop(sink);
    writer.await.expect("writer joins");
    wait_for_count(&pool, 23).await;

    let deny_latency: Option<i32> =
        sqlx::query_scalar("SELECT latency_ms FROM interaction_events WHERE outcome = 'deny'")
            .fetch_one(&pool)
            .await
            .expect("deny latency");
    assert!(deny_latency.is_none());

    let stored_token_id: Uuid = sqlx::query_scalar(
        "SELECT caller_token_id FROM interaction_events WHERE caller_kind = 'service_account'",
    )
    .fetch_one(&pool)
    .await
    .expect("service caller");
    assert_eq!(stored_token_id, token_id);

    let seqs: Vec<i64> = sqlx::query_scalar(
        "SELECT sequence_number FROM interaction_events
         WHERE session_id = $1 ORDER BY sequence_number ASC",
    )
    .bind(session_id)
    .fetch_all(&pool)
    .await
    .expect("seqs");
    assert_eq!(seqs, (1..=20).collect::<Vec<i64>>());
}

#[test]
#[ignore]
fn saturated_sink_counts_dropped_events_without_blocking() {
    let (sink, rx) = ione::services::interaction_sink::InteractionSink::new();
    let org_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    for _ in 0..4097 {
        sink.emit(event(
            org_id,
            workspace_id,
            peer_id,
            user_id,
            "allow",
            None,
            None,
        ));
    }

    drop(rx);
    assert_eq!(sink.dropped_count(), 1);
}
