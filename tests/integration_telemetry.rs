//! Funnel telemetry fidelity regression tests.
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     cargo test --test integration_telemetry -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

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

    sqlx::query(
        "TRUNCATE oauth_auth_codes, oauth_access_tokens, oauth_refresh_tokens, oauth_clients,
                  funnel_events, activation_progress, activation_dismissals,
                  audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

async fn default_ids(pool: &PgPool) -> (Uuid, Uuid) {
    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost'")
            .fetch_one(pool)
            .await
            .expect("default user");
    let workspace_id: Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations'")
            .fetch_one(pool)
            .await
            .expect("default workspace");
    (user_id, workspace_id)
}

async fn seed_real_activation_except_approval(pool: &PgPool, user_id: Uuid, workspace_id: Uuid) {
    for step in ["added_connector", "first_signal", "first_audit_viewed"] {
        sqlx::query(
            "INSERT INTO activation_progress (user_id, workspace_id, track, step_key)
             VALUES ($1, $2, 'real_activation', $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(workspace_id)
        .bind(step)
        .execute(pool)
        .await
        .expect("activation seed");
    }
}

async fn create_approval(pool: &PgPool, workspace_id: Uuid, title: &str) -> Uuid {
    let artifact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO artifacts (workspace_id, kind, content)
         VALUES ($1, 'briefing'::artifact_kind, $2)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(json!({ "title": title }))
    .fetch_one(pool)
    .await
    .expect("artifact");

    sqlx::query_scalar(
        "INSERT INTO approvals (artifact_id, status)
         VALUES ($1, 'pending'::approval_status)
         RETURNING id",
    )
    .bind(artifact_id)
    .fetch_one(pool)
    .await
    .expect("approval")
}

async fn funnel_count(pool: &PgPool, event_kind: &str, workspace_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)
         FROM funnel_events
         WHERE event_kind = $1 AND workspace_id = $2",
    )
    .bind(event_kind)
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("funnel count")
}

#[tokio::test]
#[ignore]
async fn approval_decision_and_activation_completion_emit_once() {
    let (base, pool) = spawn_app().await;
    let (user_id, workspace_id) = default_ids(&pool).await;
    seed_real_activation_except_approval(&pool, user_id, workspace_id).await;

    let approvals = [
        create_approval(&pool, workspace_id, "approval 1").await,
        create_approval(&pool, workspace_id, "approval 2").await,
        create_approval(&pool, workspace_id, "approval 3").await,
    ];

    for approval_id in approvals {
        let resp = reqwest::Client::new()
            .post(format!("{base}/api/v1/approvals/{approval_id}"))
            .json(&json!({ "decision": "rejected", "comment": "not now" }))
            .send()
            .await
            .expect("decide approval");
        assert_eq!(resp.status(), StatusCode::OK);
        let _body: Value = resp.json().await.expect("approval JSON");
    }

    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    assert_eq!(
        funnel_count(&pool, "first_real_approval_decided", workspace_id).await,
        1,
        "first approval decision funnel event must only fire once"
    );
    assert_eq!(
        funnel_count(&pool, "activation_completed", workspace_id).await,
        1,
        "activation_completed must only fire when the final missing step is first marked"
    );
}
