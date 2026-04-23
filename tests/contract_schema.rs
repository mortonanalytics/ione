//! Contract schema tests — encode the target DB schema from the ione-complete-contract.
//!
//! Every test asserts that a table, column, or constraint defined in the contract
//! exists in the live Postgres database. All tests are expected to FAIL until the
//! migrations implementing the contract are written.
//!
//! Prerequisites:
//!   docker compose up -d postgres
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
//!
//! Run:
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     cargo test --test contract_schema -- --ignored --test-threads=1

use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn pool() -> PgPool {
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
    pool
}

// ─── activation_progress ─────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_activation_progress_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'activation_progress')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table activation_progress must exist");
}

#[tokio::test]
#[ignore]
async fn schema_activation_progress_columns() {
    let pool = pool().await;

    // user_id NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'activation_progress' AND column_name = 'user_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column user_id must exist on activation_progress");
    assert_eq!(
        nullable, "NO",
        "activation_progress.user_id must be NOT NULL"
    );

    // workspace_id NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'activation_progress' AND column_name = 'workspace_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column workspace_id must exist on activation_progress");
    assert_eq!(
        nullable, "NO",
        "activation_progress.workspace_id must be NOT NULL"
    );

    // track NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'activation_progress' AND column_name = 'track'",
    )
    .fetch_one(&pool)
    .await
    .expect("column track must exist on activation_progress");
    assert_eq!(nullable, "NO", "activation_progress.track must be NOT NULL");

    // step_key NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'activation_progress' AND column_name = 'step_key'",
    )
    .fetch_one(&pool)
    .await
    .expect("column step_key must exist on activation_progress");
    assert_eq!(
        nullable, "NO",
        "activation_progress.step_key must be NOT NULL"
    );

    // completed_at NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'activation_progress' AND column_name = 'completed_at'",
    )
    .fetch_one(&pool)
    .await
    .expect("column completed_at must exist on activation_progress");
    assert_eq!(
        nullable, "NO",
        "activation_progress.completed_at must be NOT NULL"
    );
}

#[tokio::test]
#[ignore]
async fn schema_activation_progress_pk_composite() {
    // PK over (user_id, workspace_id, track, step_key): inserting a duplicate
    // must fail with a unique/pk violation.
    let pool = pool().await;
    let (uid, ws) = seed_user_and_workspace(&pool).await;

    sqlx::query(
        "INSERT INTO activation_progress (user_id, workspace_id, track, step_key, completed_at)
         VALUES ($1, $2, 'demo_walkthrough', 'asked_demo_question', now())",
    )
    .bind(uid)
    .bind(ws)
    .execute(&pool)
    .await
    .expect("first insert must succeed");

    let second = sqlx::query(
        "INSERT INTO activation_progress (user_id, workspace_id, track, step_key, completed_at)
         VALUES ($1, $2, 'demo_walkthrough', 'asked_demo_question', now())",
    )
    .bind(uid)
    .bind(ws)
    .execute(&pool)
    .await;

    assert!(
        second.is_err(),
        "duplicate PK insert must fail — composite PK not enforced"
    );
}

// ─── activation_dismissals ────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_activation_dismissals_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'activation_dismissals')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table activation_dismissals must exist");
}

#[tokio::test]
#[ignore]
async fn schema_activation_dismissals_columns() {
    let pool = pool().await;

    for col in &["user_id", "workspace_id", "track", "dismissed_at"] {
        let nullable: String = sqlx::query_scalar(
            "SELECT is_nullable FROM information_schema.columns
             WHERE table_name = 'activation_dismissals' AND column_name = $1",
        )
        .bind(*col)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|_| panic!("column {} must exist on activation_dismissals", col));
        assert_eq!(
            nullable, "NO",
            "activation_dismissals.{} must be NOT NULL",
            col
        );
    }
}

#[tokio::test]
#[ignore]
async fn schema_activation_dismissals_pk_composite() {
    let pool = pool().await;
    let (uid, ws) = seed_user_and_workspace(&pool).await;

    sqlx::query(
        "INSERT INTO activation_dismissals (user_id, workspace_id, track, dismissed_at)
         VALUES ($1, $2, 'real_activation', now())",
    )
    .bind(uid)
    .bind(ws)
    .execute(&pool)
    .await
    .expect("first insert must succeed");

    let second = sqlx::query(
        "INSERT INTO activation_dismissals (user_id, workspace_id, track, dismissed_at)
         VALUES ($1, $2, 'real_activation', now())",
    )
    .bind(uid)
    .bind(ws)
    .execute(&pool)
    .await;

    assert!(
        second.is_err(),
        "duplicate PK (user_id, workspace_id, track) must be rejected"
    );
}

// ─── pipeline_events ─────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_pipeline_events_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'pipeline_events')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table pipeline_events must exist");
}

#[tokio::test]
#[ignore]
async fn schema_pipeline_events_columns() {
    let pool = pool().await;

    // id NOT NULL
    let _: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column id must exist on pipeline_events");

    // workspace_id NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'workspace_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column workspace_id must exist on pipeline_events");
    assert_eq!(
        nullable, "NO",
        "pipeline_events.workspace_id must be NOT NULL"
    );

    // connector_id nullable
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'connector_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column connector_id must exist on pipeline_events");
    assert_eq!(
        nullable, "YES",
        "pipeline_events.connector_id must be NULL-able"
    );

    // stream_id nullable
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'stream_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column stream_id must exist on pipeline_events");
    assert_eq!(
        nullable, "YES",
        "pipeline_events.stream_id must be NULL-able"
    );

    // stage NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'stage'",
    )
    .fetch_one(&pool)
    .await
    .expect("column stage must exist on pipeline_events");
    assert_eq!(nullable, "NO", "pipeline_events.stage must be NOT NULL");

    // detail nullable JSONB
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'detail'",
    )
    .fetch_one(&pool)
    .await
    .expect("column detail must exist on pipeline_events");
    assert_eq!(nullable, "YES", "pipeline_events.detail must be NULL-able");

    // occurred_at NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'pipeline_events' AND column_name = 'occurred_at'",
    )
    .fetch_one(&pool)
    .await
    .expect("column occurred_at must exist on pipeline_events");
    assert_eq!(
        nullable, "NO",
        "pipeline_events.occurred_at must be NOT NULL"
    );
}

// ─── funnel_events ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_funnel_events_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'funnel_events')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table funnel_events must exist");
}

#[tokio::test]
#[ignore]
async fn schema_funnel_events_columns() {
    let pool = pool().await;

    // id NOT NULL
    let _: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column id must exist on funnel_events");

    // user_id nullable
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'user_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column user_id must exist on funnel_events");
    assert_eq!(nullable, "YES", "funnel_events.user_id must be NULL-able");

    // session_id NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'session_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column session_id must exist on funnel_events");
    assert_eq!(nullable, "NO", "funnel_events.session_id must be NOT NULL");

    // workspace_id nullable
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'workspace_id'",
    )
    .fetch_one(&pool)
    .await
    .expect("column workspace_id must exist on funnel_events");
    assert_eq!(
        nullable, "YES",
        "funnel_events.workspace_id must be NULL-able"
    );

    // event_kind NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'event_kind'",
    )
    .fetch_one(&pool)
    .await
    .expect("column event_kind must exist on funnel_events");
    assert_eq!(nullable, "NO", "funnel_events.event_kind must be NOT NULL");

    // detail nullable JSONB
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'detail'",
    )
    .fetch_one(&pool)
    .await
    .expect("column detail must exist on funnel_events");
    assert_eq!(nullable, "YES", "funnel_events.detail must be NULL-able");

    // occurred_at NOT NULL
    let nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'funnel_events' AND column_name = 'occurred_at'",
    )
    .fetch_one(&pool)
    .await
    .expect("column occurred_at must exist on funnel_events");
    assert_eq!(nullable, "NO", "funnel_events.occurred_at must be NOT NULL");
}

// ─── OAuth tables ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_oauth_clients_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'oauth_clients')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table oauth_clients must exist");
}

#[tokio::test]
#[ignore]
async fn schema_oauth_auth_codes_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'oauth_auth_codes')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table oauth_auth_codes must exist");
}

#[tokio::test]
#[ignore]
async fn schema_oauth_access_tokens_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'oauth_access_tokens')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table oauth_access_tokens must exist");
}

#[tokio::test]
#[ignore]
async fn schema_oauth_refresh_tokens_table_exists() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'oauth_refresh_tokens')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "table oauth_refresh_tokens must exist");
}

// ─── peers extended columns ────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_oauth_client_id() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_name = 'peers' AND column_name = 'oauth_client_id')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "peers.oauth_client_id must exist");
}

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_access_token_hash() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_name = 'peers' AND column_name = 'access_token_hash')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "peers.access_token_hash must exist");
}

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_refresh_token_hash() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_name = 'peers' AND column_name = 'refresh_token_hash')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "peers.refresh_token_hash must exist");
}

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_token_expires_at() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_name = 'peers' AND column_name = 'token_expires_at')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "peers.token_expires_at must exist");
}

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_tool_allowlist() {
    let pool = pool().await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns
         WHERE table_name = 'peers' AND column_name = 'tool_allowlist')",
    )
    .fetch_one(&pool)
    .await
    .expect("query failed");
    assert!(exists, "peers.tool_allowlist must exist (JSONB)");
}

#[tokio::test]
#[ignore]
async fn schema_peers_extended_column_status_new_variants() {
    // The contract extends peer_status to include pending_oauth and pending_allowlist.
    // Current peers table has peer_status ('active','paused','error').
    // This test inserts each new variant; it fails until the enum is extended.
    let pool = pool().await;

    // We use a raw cast to check the enum accepts the variant.
    let result: Result<_, sqlx::Error> = sqlx::query("SELECT 'pending_oauth'::peer_status AS s")
        .execute(&pool)
        .await;
    assert!(
        result.is_ok(),
        "peer_status must include 'pending_oauth' variant"
    );

    let result: Result<_, sqlx::Error> =
        sqlx::query("SELECT 'pending_allowlist'::peer_status AS s")
            .execute(&pool)
            .await;
    assert!(
        result.is_ok(),
        "peer_status must include 'pending_allowlist' variant"
    );

    let result: Result<_, sqlx::Error> = sqlx::query("SELECT 'revoked'::peer_status AS s")
        .execute(&pool)
        .await;
    assert!(result.is_ok(), "peer_status must include 'revoked' variant");
}

// ─── activation_track enum (TEXT + CHECK constraint) ─────────────────────────

async fn seed_user_and_workspace(pool: &PgPool) -> (Uuid, Uuid) {
    let org_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let ws_id = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'activation-test-org')")
        .bind(org_id)
        .execute(pool)
        .await
        .expect("insert org");
    sqlx::query("INSERT INTO users (id, org_id, email, display_name) VALUES ($1, $2, $3, 'test')")
        .bind(user_id)
        .bind(org_id)
        .bind(format!("test-{user_id}@example.com"))
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query("INSERT INTO workspaces (id, org_id, name, domain, lifecycle) VALUES ($1, $2, 'act-test-ws', 'generic', 'continuous')")
        .bind(ws_id).bind(org_id).execute(pool).await.expect("insert workspace");
    (user_id, ws_id)
}

async fn try_insert_activation_track(pool: &PgPool, track: &str) -> Result<(), sqlx::Error> {
    let (uid, ws) = seed_user_and_workspace(pool).await;
    sqlx::query("INSERT INTO activation_progress (user_id, workspace_id, track, step_key) VALUES ($1, $2, $3, 'asked_demo_question')")
        .bind(uid).bind(ws).bind(track).execute(pool).await.map(|_| ())
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_track_demo_walkthrough() {
    let pool = pool().await;
    try_insert_activation_track(&pool, "demo_walkthrough")
        .await
        .expect("activation_track must accept 'demo_walkthrough'");
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_track_real_activation() {
    let pool = pool().await;
    try_insert_activation_track(&pool, "real_activation")
        .await
        .expect("activation_track must accept 'real_activation'");
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_track_rejects_junk() {
    let pool = pool().await;
    let result = try_insert_activation_track(&pool, "not_a_track").await;
    assert!(
        result.is_err(),
        "activation_track must reject unknown variant"
    );
}

// ─── activation_step_key CHECK constraint ────────────────────────────────────

async fn try_insert_step_key(pool: &PgPool, step: &str) -> Result<(), sqlx::Error> {
    let (uid, ws) = seed_user_and_workspace(pool).await;
    let track = if step.contains("demo") {
        "demo_walkthrough"
    } else {
        "real_activation"
    };
    sqlx::query("INSERT INTO activation_progress (user_id, workspace_id, track, step_key) VALUES ($1, $2, $3, $4)")
        .bind(uid).bind(ws).bind(track).bind(step).execute(pool).await.map(|_| ())
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_step_key_demo_variants() {
    let pool = pool().await;
    for variant in &[
        "asked_demo_question",
        "opened_demo_survivor",
        "reviewed_demo_approval",
        "viewed_demo_audit",
    ] {
        try_insert_step_key(&pool, variant)
            .await
            .unwrap_or_else(|_| panic!("activation_step_key must accept demo variant '{variant}'"));
    }
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_step_key_real_variants() {
    let pool = pool().await;
    for variant in &[
        "added_connector",
        "first_signal",
        "first_approval_decided",
        "first_audit_viewed",
    ] {
        try_insert_step_key(&pool, variant)
            .await
            .unwrap_or_else(|_| panic!("activation_step_key must accept real variant '{variant}'"));
    }
}

#[tokio::test]
#[ignore]
async fn schema_enum_activation_step_key_rejects_junk() {
    let pool = pool().await;
    let result = try_insert_step_key(&pool, "not_a_step").await;
    assert!(
        result.is_err(),
        "activation_step_key must reject unknown variant"
    );
}

// ─── pipeline_event_stage enum ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_enum_pipeline_event_stage_all_variants() {
    let pool = pool().await;
    // 7 stages from contract: publish_started, first_event, first_signal,
    // first_survivor, first_decision, stall, error. Stored as TEXT with a
    // CHECK constraint (not a Postgres ENUM), so assert by inserting a row
    // with each stage value and confirming success.
    use uuid::Uuid;
    let ws_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO organizations (id, name) VALUES ($1, 'stage-test-org') ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .ok();
    let org_id: Uuid =
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'stage-test-org'")
            .fetch_one(&pool)
            .await
            .expect("create org");
    sqlx::query("INSERT INTO workspaces (id, org_id, name, domain, lifecycle) VALUES ($1, $2, 'stage-test-ws', 'generic', 'continuous')")
        .bind(ws_id).bind(org_id).execute(&pool).await.expect("create workspace");
    for variant in &[
        "publish_started",
        "first_event",
        "first_signal",
        "first_survivor",
        "first_decision",
        "stall",
        "error",
    ] {
        let result: Result<_, sqlx::Error> =
            sqlx::query("INSERT INTO pipeline_events (workspace_id, stage) VALUES ($1, $2)")
                .bind(ws_id)
                .bind(*variant)
                .execute(&pool)
                .await;
        assert!(
            result.is_ok(),
            "pipeline_event_stage must have variant '{}'",
            variant
        );
    }
}

#[tokio::test]
#[ignore]
async fn schema_enum_pipeline_event_stage_rejects_junk() {
    let pool = pool().await;
    let result: Result<_, sqlx::Error> =
        sqlx::query("SELECT 'not_a_stage'::pipeline_event_stage AS s")
            .execute(&pool)
            .await;
    assert!(
        result.is_err(),
        "pipeline_event_stage must reject unknown variant"
    );
}

// ─── peer_status enum full set ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn schema_enum_peer_status_all_variants() {
    let pool = pool().await;
    for variant in &["pending_oauth", "pending_allowlist", "active", "revoked"] {
        let result: Result<_, sqlx::Error> =
            sqlx::query(&format!("SELECT '{}'::peer_status AS s", variant))
                .execute(&pool)
                .await;
        assert!(
            result.is_ok(),
            "peer_status must have variant '{}'",
            variant
        );
    }
}
