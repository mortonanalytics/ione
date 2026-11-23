//! Proves that the org-isolation RLS policies actually filter rows — the
//! positive counterpart to
//! `identity_broker_integration::rls_policies_are_present_but_inert_as_deployed`,
//! which pins what is still inert under the default connection.
//!
//! Enforcement needs three things, and this suite checks all three:
//!   1. `FORCE ROW LEVEL SECURITY` on every org-scoped table (migration 0050);
//!   2. a connection role that is neither SUPERUSER nor BYPASSRLS (`ione_app`,
//!      created by the same migration);
//!   3. `app.current_org_id` set per transaction (`ione::rls::org_scoped_tx`).
//!
//! The default `ione` role is SUPERUSER and still bypasses everything, which is
//! exactly why adding all of this breaks no existing test — asserted here
//! directly rather than assumed.
//!
//! Run: cargo test --test rls_enforcement_integration -- --ignored --test-threads=1

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// The restricted role and its bootstrap password, both fixed by migration 0050.
const RESTRICTED_ROLE: &str = "ione_app";
const RESTRICTED_PASSWORD: &str = "ione_app";

/// Every table carrying an `org_id` RLS policy. Kept in sync with migration 0050
/// by the `force_row_level_security` test below.
const GUARDED_TABLES: [&str; 11] = [
    "auto_exec_policies",
    "broker_credentials",
    "identity_audit_events",
    "interaction_events",
    "mfa_enrollments",
    "mfa_recovery_codes",
    "peer_catalog_entries",
    "service_account_tokens",
    "workspace_peer_bindings",
    "workspace_peer_credentials",
    "workspace_peer_delegations",
];

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned())
}

/// The pool the application uses today: role `ione`, SUPERUSER + BYPASSRLS.
async fn default_pool() -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url())
        .await
        .expect("failed to connect to Postgres as the default role");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");
    pool
}

/// A second pool on the same database as the restricted role. Same host, port,
/// and database as `DATABASE_URL`; only the credentials differ, so this works
/// against any developer or CI database without new configuration.
async fn restricted_pool() -> PgPool {
    let options = PgConnectOptions::from_str(&database_url())
        .expect("DATABASE_URL is not a valid Postgres URL")
        .username(RESTRICTED_ROLE)
        .password(RESTRICTED_PASSWORD);
    PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("failed to connect to Postgres as the restricted role")
}

/// Two organizations, one user each, one broker credential each. `provider` is
/// unique per run so counts are unaffected by rows other suites leave behind.
struct TwoOrgs {
    org_a: Uuid,
    org_b: Uuid,
    user_a: Uuid,
    user_b: Uuid,
    provider: String,
}

async fn seed_two_orgs(pool: &PgPool) -> TwoOrgs {
    let provider = format!("rls-probe-{}", Uuid::new_v4());
    let mut ids = Vec::new();
    for label in ["A", "B"] {
        let org_id: Uuid =
            sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id")
                .bind(format!("RLS Enforcement Org {label} {provider}"))
                .fetch_one(pool)
                .await
                .expect("insert organization");
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (org_id, email, display_name) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(org_id)
        .bind(format!("{provider}-{label}@example.test"))
        .bind(format!("RLS Probe {label}"))
        .fetch_one(pool)
        .await
        .expect("insert user");
        sqlx::query(
            "INSERT INTO broker_credentials (user_id, org_id, provider, label)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(org_id)
        .bind(&provider)
        .bind(format!("org-{label}-secret"))
        .execute(pool)
        .await
        .expect("insert broker credential");
        ids.push((org_id, user_id));
    }
    TwoOrgs {
        org_a: ids[0].0,
        org_b: ids[1].0,
        user_a: ids[0].1,
        user_b: ids[1].1,
        provider,
    }
}

/// `SELECT count(*)` with no `org_id` predicate: the only thing that can filter
/// these rows is the RLS policy, which is the point.
async fn count_unfiltered(executor: impl sqlx::PgExecutor<'_>, provider: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE provider = $1")
        .bind(provider)
        .fetch_one(executor)
        .await
        .expect("count broker credentials")
}

#[tokio::test]
#[ignore]
async fn force_row_level_security_is_set_on_every_org_scoped_table() {
    let pool = default_pool().await;
    for table in GUARDED_TABLES {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT c.relrowsecurity, c.relforcerowsecurity
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public' AND c.relname = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("relation flags");
        assert!(enabled, "{table} does not have RLS enabled");
        assert!(forced, "{table} does not FORCE RLS");

        let policies: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_policies WHERE tablename = $1")
                .bind(table)
                .fetch_one(&pool)
                .await
                .expect("policy count");
        assert!(policies > 0, "{table} has no RLS policy to force");
    }
}

#[tokio::test]
#[ignore]
async fn restricted_role_holds_neither_superuser_nor_bypassrls() {
    let pool = default_pool().await;
    let (superuser, bypassrls, can_login): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, rolcanlogin FROM pg_roles WHERE rolname = $1",
    )
    .bind(RESTRICTED_ROLE)
    .fetch_one(&pool)
    .await
    .expect("restricted role should exist after migration 0050");
    assert!(
        !superuser,
        "{RESTRICTED_ROLE} is SUPERUSER — RLS cannot apply"
    );
    assert!(
        !bypassrls,
        "{RESTRICTED_ROLE} holds BYPASSRLS — RLS cannot apply"
    );
    assert!(can_login, "{RESTRICTED_ROLE} cannot log in");

    // It must also own nothing: an owner without FORCE would bypass policies.
    let owned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND pg_get_userbyid(c.relowner) = $1",
    )
    .bind(RESTRICTED_ROLE)
    .fetch_one(&pool)
    .await
    .expect("owned relation count");
    assert_eq!(owned, 0, "{RESTRICTED_ROLE} owns relations in public");
}

#[tokio::test]
#[ignore]
async fn restricted_role_sees_only_the_org_in_context() {
    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    // Org A's context: org B's row is invisible even with no org predicate.
    let mut tx = ione::rls::org_scoped_tx(&restricted, seed.org_a)
        .await
        .expect("org-scoped transaction for org A");
    let visible_to_a = count_unfiltered(&mut *tx, &seed.provider).await;
    let foreign_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE org_id = $1")
            .bind(seed.org_b)
            .fetch_one(&mut *tx)
            .await
            .expect("count org B rows under org A context");
    tx.commit().await.expect("commit org A transaction");
    assert_eq!(
        visible_to_a, 1,
        "org A context should see exactly its own seeded credential"
    );
    assert_eq!(foreign_rows, 0, "org B's rows leaked into org A's context");

    // Symmetrically for org B, so the result is isolation and not an accident of
    // ordering or of the policy failing open in one direction.
    let mut tx = ione::rls::org_scoped_tx(&restricted, seed.org_b)
        .await
        .expect("org-scoped transaction for org B");
    let visible_to_b = count_unfiltered(&mut *tx, &seed.provider).await;
    tx.commit().await.expect("commit org B transaction");
    assert_eq!(
        visible_to_b, 1,
        "org B context should see its own credential"
    );

    // Without any org context the restricted role fails closed and returns
    // nothing. Migration 0051 made that true of both shapes the setting takes:
    // NULL on a fresh connection, and the empty string on one recycled from an
    // org-scoped transaction. Before it, the empty string reached `''::uuid`
    // and raised 22P02 from inside the policy.
    let without_context: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE provider = $1")
            .bind(&seed.provider)
            .fetch_one(&restricted)
            .await
            .expect("no org context must return no rows, not a database error");
    assert_eq!(
        without_context, 0,
        "restricted role saw rows with no org context set"
    );

    // And the default role — the one every existing test and the dev loop use —
    // is untouched: it is SUPERUSER, so it still sees both orgs.
    let (default_super, default_bypass): (bool, bool) =
        sqlx::query_as("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&pool)
            .await
            .expect("default role attributes");
    assert!(
        default_super || default_bypass,
        "the default role no longer bypasses RLS — existing suites would now be org-filtered"
    );
    let visible_to_default = count_unfiltered(&pool, &seed.provider).await;
    assert_eq!(
        visible_to_default, 2,
        "the default role should still see both orgs' rows"
    );

    cleanup(&pool, &seed).await;
}

/// The empty-string shape specifically. A pooled connection that carried an
/// org-scoped transaction leaves `app.current_org_id` defined and empty once
/// that transaction ends, and `''::uuid` used to raise 22P02 from inside every
/// policy. Fail-closed either way, but an opaque database error on every
/// not-yet-migrated query is what kept `ione_app` from being a runnable role.
///
/// This is the test that fails without migration 0051.
#[tokio::test]
#[ignore]
async fn an_empty_org_context_returns_no_rows_rather_than_raising() {
    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let mut tx = restricted
        .begin()
        .await
        .expect("begin transaction on the restricted pool");
    sqlx::query("SELECT set_config('app.current_org_id', '', true)")
        .execute(&mut *tx)
        .await
        .expect("set an empty org context");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broker_credentials WHERE provider = $1")
            .bind(&seed.provider)
            .fetch_one(&mut *tx)
            .await
            .expect("an empty org context must filter, not raise 22P02");
    assert_eq!(count, 0, "an empty org context must see no rows");

    tx.commit().await.expect("commit");
    cleanup(&pool, &seed).await;
}

/// The credential and delegation reads that #25 threaded an org id through must
/// work under the restricted role, where RLS is the only filter. Before the
/// change these queries selected on (workspace, peer) with no org context at
/// all, so under `ione_app` they would have matched nothing.
#[tokio::test]
#[ignore]
async fn threaded_credential_and_delegation_reads_work_under_the_restricted_role() {
    // The credential is stored encrypted; the repo needs a key like any caller.
    std::env::set_var(
        "IONE_TOKEN_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, 'rls-threaded', 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(seed.org_a)
    .fetch_one(&pool)
    .await
    .expect("insert workspace");

    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(format!("https://issuer-{workspace_id}.example"))
    .fetch_one(&pool)
    .await
    .expect("insert trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ($1, 'rls-peer', $3, $2, '{}'::jsonb, '[]'::jsonb,
                 'active'::peer_status)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(issuer_id)
    .bind(format!("https://peer-{workspace_id}.example/mcp"))
    .fetch_one(&pool)
    .await
    .expect("insert peer");

    let repo = ione::repos::WorkspacePeerCredentialRepo::new(restricted.clone());
    repo.upsert(seed.org_a, workspace_id, peer_id, "s3cret", None)
        .await
        .expect("upsert under the restricted role");
    let secret = repo
        .secret_for(seed.org_a, workspace_id, peer_id)
        .await
        .expect("read under the restricted role");
    assert_eq!(
        secret.as_deref(),
        Some("s3cret"),
        "the threaded org id should make this row visible under RLS"
    );

    // The other org's context must not see it, which is the isolation half.
    let wrong_org = repo
        .secret_for(seed.org_b, workspace_id, peer_id)
        .await
        .expect("read with the wrong org context");
    assert_eq!(
        wrong_org, None,
        "org B's context must not read org A's peer credential"
    );

    sqlx::query("DELETE FROM workspace_peer_credentials WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(&pool)
        .await
        .expect("cleanup credentials");
    sqlx::query("DELETE FROM peers WHERE id = $1")
        .bind(peer_id)
        .execute(&pool)
        .await
        .expect("cleanup peer");
    sqlx::query("DELETE FROM trust_issuers WHERE id = $1")
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("cleanup issuer");
    sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .execute(&pool)
        .await
        .expect("cleanup workspace");
    cleanup(&pool, &seed).await;
}

/// The migrated repository methods must keep working under the restricted role.
/// They only do so because `org_scoped_tx` sets the context: without it, RLS
/// would filter every row away and these calls would return nothing.
#[tokio::test]
#[ignore]
async fn migrated_broker_repo_works_under_the_restricted_role() {
    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;
    let repo = ione::repos::BrokerCredentialRepo::new(restricted.clone());

    let mine = repo
        .list_for_user(seed.user_a, seed.org_a)
        .await
        .expect("list own broker credentials under the restricted role");
    assert_eq!(
        mine.len(),
        1,
        "the migrated read path returned nothing under the restricted role"
    );
    assert_eq!(mine[0].org_id, seed.org_a);

    // Cross-org: user B's row, addressed with org A's context, is not reachable.
    let cross = repo
        .list_for_user(seed.user_b, seed.org_a)
        .await
        .expect("cross-org list should succeed and be empty");
    assert!(cross.is_empty(), "cross-org read returned rows");

    // The write path passes the policy's WITH CHECK because the org context and
    // the inserted org_id agree.
    let created = repo
        .create_pending(
            seed.user_b,
            seed.org_b,
            &format!("{}-write", seed.provider),
            "",
            &["read".to_string()],
            &format!("state-{}", Uuid::new_v4()),
            "verifier",
            chrono::Utc::now() + chrono::Duration::minutes(10),
        )
        .await
        .expect("insert under the restricted role");
    assert_eq!(created.org_id, seed.org_b);

    cleanup(&pool, &seed).await;
}

/// `interaction_events` writes and reads must work under the restricted role.
/// The policy has no separate `WITH CHECK`, so before the threading the batch
/// insert was rejected outright and every read returned nothing. The batch here
/// mixes two orgs on purpose: the writer flushes whatever the bus delivered, so
/// one batch can span orgs and each org's rows need their own context.
#[tokio::test]
#[ignore]
async fn interaction_event_repo_works_under_the_restricted_role() {
    use ione::models::{ActorKind, InteractionEvent};
    use ione::repos::{InteractionEventFilter, InteractionEventRepo};

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let mut workspaces = Vec::new();
    for org_id in [seed.org_a, seed.org_b] {
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspaces (org_id, name, domain, lifecycle)
             VALUES ($1, 'rls-interactions', 'test', 'continuous'::workspace_lifecycle)
             RETURNING id",
        )
        .bind(org_id)
        .fetch_one(&pool)
        .await
        .expect("insert workspace");
        workspaces.push(workspace_id);
    }
    let (workspace_a, workspace_b) = (workspaces[0], workspaces[1]);

    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(format!("https://issuer-{workspace_a}.example"))
    .fetch_one(&pool)
    .await
    .expect("insert trust issuer");
    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ($1, 'rls-interaction-peer', $3, $2, '{}'::jsonb, '[]'::jsonb,
                 'active'::peer_status)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(issuer_id)
    .bind(format!("https://peer-{workspace_a}.example/mcp"))
    .fetch_one(&pool)
    .await
    .expect("insert peer");

    let event = |org_id: Uuid, workspace_id: Uuid, user_id: Uuid| InteractionEvent {
        id: Uuid::new_v4(),
        org_id,
        workspace_id,
        peer_id,
        peer_name: "rls-interaction-peer".into(),
        tool_name: "probe".into(),
        caller_kind: ActorKind::User,
        caller_user_id: Some(user_id),
        caller_peer_id: None,
        caller_token_id: None,
        session_id: None,
        sequence_number: None,
        outcome: "allow".into(),
        latency_ms: Some(1),
        detail: serde_json::json!({}),
        recorded_at: chrono::Utc::now(),
    };

    let repo = InteractionEventRepo::new(restricted.clone());
    let written = repo
        .insert_batch(&[
            event(seed.org_a, workspace_a, seed.user_a),
            event(seed.org_b, workspace_b, seed.user_b),
        ])
        .await
        .expect("a mixed-org batch must insert under the restricted role");
    assert_eq!(written, 2, "both orgs' rows should be written");

    let filter = InteractionEventFilter::default();
    for (org_id, workspace_id) in [(seed.org_a, workspace_a), (seed.org_b, workspace_b)] {
        let rows = repo
            .list_filtered(workspace_id, org_id, &filter, None, 50)
            .await
            .expect("list under the restricted role");
        assert_eq!(rows.len(), 1, "the org's own interaction should be visible");
        assert_eq!(rows[0].org_id, org_id);

        let summary = repo
            .outcome_summary(workspace_id, org_id, &filter)
            .await
            .expect("summary under the restricted role");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].count, 1);
    }

    // RLS alone, with no org predicate in the SQL: org B's context sees only
    // org B's row, never org A's.
    let mut tx = ione::rls::org_scoped_tx(&restricted, seed.org_b)
        .await
        .expect("org B context");
    let visible: Vec<Uuid> =
        sqlx::query_scalar("SELECT org_id FROM interaction_events WHERE workspace_id = ANY($1)")
            .bind(vec![workspace_a, workspace_b])
            .fetch_all(&mut *tx)
            .await
            .expect("unfiltered read");
    assert_eq!(
        visible,
        vec![seed.org_b],
        "org B's context must not see org A's interaction"
    );
    tx.commit().await.expect("commit");

    sqlx::query("DELETE FROM interaction_events WHERE workspace_id = ANY($1)")
        .bind(vec![workspace_a, workspace_b])
        .execute(&pool)
        .await
        .expect("cleanup interactions");
    sqlx::query("DELETE FROM peers WHERE id = $1")
        .bind(peer_id)
        .execute(&pool)
        .await
        .expect("cleanup peer");
    sqlx::query("DELETE FROM trust_issuers WHERE id = $1")
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("cleanup issuer");
    sqlx::query("DELETE FROM workspaces WHERE id = ANY($1)")
        .bind(vec![workspace_a, workspace_b])
        .execute(&pool)
        .await
        .expect("cleanup workspaces");
    cleanup(&pool, &seed).await;
}

/// `identity_audit_events` writes must land under the restricted role. The
/// policy has no separate `WITH CHECK`, so an insert without the org context is
/// rejected outright, and an identity audit row lost that way is a login or MFA
/// event with no record.
#[tokio::test]
#[ignore]
async fn identity_audit_writer_works_under_the_restricted_role() {
    use ione::services::{IdentityAuditWriter, IdentityEvent};

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let writer = IdentityAuditWriter::new(&restricted);
    for (org_id, user_id) in [(seed.org_a, seed.user_a), (seed.org_b, seed.user_b)] {
        writer
            .write(
                IdentityEvent::OidcLogin,
                org_id,
                Some(user_id),
                None,
                None,
                None,
                "success",
                serde_json::json!({ "probe": seed.provider }),
            )
            .await
            .expect("identity audit write must succeed under the restricted role");
    }

    // RLS alone, with no org predicate in the SQL: each org's context sees only
    // its own audit row.
    for (org_id, user_id) in [(seed.org_a, seed.user_a), (seed.org_b, seed.user_b)] {
        let mut tx = ione::rls::org_scoped_tx(&restricted, org_id)
            .await
            .expect("org context");
        let visible: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT org_id, user_id FROM identity_audit_events WHERE detail->>'probe' = $1",
        )
        .bind(&seed.provider)
        .fetch_all(&mut *tx)
        .await
        .expect("unfiltered read");
        assert_eq!(
            visible,
            vec![(org_id, Some(user_id))],
            "an org's context must see its own audit row and not the other org's"
        );
        tx.commit().await.expect("commit");
    }

    sqlx::query("DELETE FROM identity_audit_events WHERE org_id = ANY($1)")
        .bind(vec![seed.org_a, seed.org_b])
        .execute(&pool)
        .await
        .expect("cleanup identity audit events");
    cleanup(&pool, &seed).await;
}

/// Every `AutoExecPolicyRepo` method must work under the restricted role, and
/// another org's context must never reach the policy. Unscoped, the list reads
/// return nothing (the engine silently stops auto-executing) and the create is
/// rejected by the policy's implicit insert check.
#[tokio::test]
#[ignore]
async fn auto_exec_policy_repo_works_under_the_restricted_role() {
    use ione::repos::{AutoExecPolicyInput, AutoExecPolicyRepo};

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let mut workspaces = Vec::new();
    for org_id in [seed.org_a, seed.org_b] {
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspaces (org_id, name, domain, lifecycle)
             VALUES ($1, 'rls-auto-exec', 'test', 'continuous'::workspace_lifecycle)
             RETURNING id",
        )
        .bind(org_id)
        .fetch_one(&pool)
        .await
        .expect("insert workspace");
        workspaces.push(workspace_id);
    }
    let (workspace_a, workspace_b) = (workspaces[0], workspaces[1]);
    let connector_id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, 'rls-auto-exec', '{}'::jsonb)
         RETURNING id",
    )
    .bind(workspace_a)
    .fetch_one(&pool)
    .await
    .expect("insert connector");

    let input = |enabled: bool| AutoExecPolicyInput {
        name: "rls-auto-exec".into(),
        trigger_signal_title_prefix: None,
        trigger_severity_at_most: None,
        connector_id,
        op: "probe".into(),
        args_template: serde_json::json!({}),
        rate_limit_per_min: 1,
        severity_cap: "routine".into(),
        authorized_by_permission: "approvals:decide".into(),
        enabled,
    };

    let repo = AutoExecPolicyRepo::new(restricted.clone());
    let created = repo
        .create(seed.org_a, workspace_a, seed.user_a, &input(true))
        .await
        .expect("create must succeed under the restricted role");
    assert_eq!(created.org_id, seed.org_a);

    let listed = repo
        .list_for_workspace(seed.org_a, workspace_a)
        .await
        .expect("list");
    assert_eq!(
        listed.iter().map(|p| p.id).collect::<Vec<_>>(),
        [created.id]
    );
    let enabled = repo
        .list_enabled_for_workspace(seed.org_a, workspace_a)
        .await
        .expect("list enabled");
    assert_eq!(
        enabled.iter().map(|p| p.id).collect::<Vec<_>>(),
        [created.id]
    );
    assert!(repo
        .get(seed.org_a, created.id, workspace_a)
        .await
        .expect("get")
        .is_some());
    let updated = repo
        .update(seed.org_a, created.id, workspace_a, &input(false))
        .await
        .expect("update")
        .expect("update must find the policy");
    assert!(!updated.enabled);

    // The other org's context sees nothing and changes nothing, even when the
    // caller names the right workspace and policy id.
    assert!(repo
        .list_for_workspace(seed.org_b, workspace_a)
        .await
        .expect("foreign list")
        .is_empty());
    assert!(repo
        .get(seed.org_b, created.id, workspace_a)
        .await
        .expect("foreign get")
        .is_none());
    assert!(repo
        .update(seed.org_b, created.id, workspace_a, &input(true))
        .await
        .expect("foreign update")
        .is_none());
    assert!(!repo
        .delete(seed.org_b, created.id, workspace_a)
        .await
        .expect("foreign delete"));
    // Nor can it create a policy in a workspace that belongs to org A.
    assert!(repo
        .create(seed.org_b, workspace_a, seed.user_b, &input(true))
        .await
        .is_err());
    assert!(repo
        .list_for_workspace(seed.org_b, workspace_b)
        .await
        .expect("org B list")
        .is_empty());

    assert!(repo
        .delete(seed.org_a, created.id, workspace_a)
        .await
        .expect("delete"));
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auto_exec_policies WHERE workspace_id = $1")
            .bind(workspace_a)
            .fetch_one(&pool)
            .await
            .expect("count remaining");
    assert_eq!(remaining, 0);

    sqlx::query("DELETE FROM connectors WHERE id = $1")
        .bind(connector_id)
        .execute(&pool)
        .await
        .expect("cleanup connector");
    sqlx::query("DELETE FROM workspaces WHERE id = ANY($1)")
        .bind(vec![workspace_a, workspace_b])
        .execute(&pool)
        .await
        .expect("cleanup workspaces");
    cleanup(&pool, &seed).await;
}

/// Every `MfaRepo` method must work under the restricted role, and another
/// org's context must never reach the enrollment or its recovery codes.
/// Unscoped, the enrollment insert is rejected by the policy's implicit insert
/// check, and `is_enrolled` reads `false` — which is the dangerous half: the MFA
/// gate would wave an enrolled user through.
#[tokio::test]
#[ignore]
async fn mfa_repo_works_under_the_restricted_role() {
    use ione::repos::MfaRepo;

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;
    let repo = MfaRepo::new(restricted.clone());
    let (org_a, user_a, org_b) = (seed.org_a, seed.user_a, seed.org_b);

    repo.upsert_enrollment(org_a, user_a, b"ciphertext")
        .await
        .expect("enroll must succeed under the restricted role");
    assert!(!repo.is_enrolled(org_a, user_a).await.expect("is_enrolled"));
    assert_eq!(
        repo.totp_ciphertext(org_a, user_a)
            .await
            .expect("totp_ciphertext")
            .as_deref(),
        Some(&b"ciphertext"[..])
    );
    repo.activate(org_a, user_a).await.expect("activate");
    assert!(repo.is_enrolled(org_a, user_a).await.expect("is_enrolled"));

    assert!(repo
        .recovery_codes_viewed_at(org_a, user_a)
        .await
        .expect("viewed_at")
        .is_none());
    let hashes = vec!["hash-1".to_string(), "hash-2".to_string()];
    repo.store_recovery_codes(org_a, user_a, &hashes)
        .await
        .expect("store recovery codes");
    assert!(repo
        .recovery_codes_viewed_at(org_a, user_a)
        .await
        .expect("viewed_at")
        .is_some());
    assert_eq!(
        repo.unused_recovery_count(org_a, user_a)
            .await
            .expect("count"),
        2
    );

    // The other org's context sees nothing and consumes nothing, even with the
    // right user id and a valid code hash.
    assert!(!repo.is_enrolled(org_b, user_a).await.expect("foreign"));
    assert!(repo
        .totp_ciphertext(org_b, user_a)
        .await
        .expect("foreign")
        .is_none());
    assert_eq!(
        repo.unused_recovery_count(org_b, user_a)
            .await
            .expect("foreign"),
        0
    );
    assert!(!repo
        .consume_recovery_code(org_b, user_a, "hash-1")
        .await
        .expect("foreign consume"));
    repo.delete_enrollment(org_b, user_a)
        .await
        .expect("foreign delete");
    assert!(repo
        .is_enrolled(org_a, user_a)
        .await
        .expect("still enrolled"));

    assert!(repo
        .consume_recovery_code(org_a, user_a, "hash-1")
        .await
        .expect("consume"));
    assert!(!repo
        .consume_recovery_code(org_a, user_a, "hash-1")
        .await
        .expect("reuse"));
    assert_eq!(
        repo.unused_recovery_count(org_a, user_a)
            .await
            .expect("count"),
        1
    );

    repo.delete_enrollment(org_a, user_a).await.expect("delete");
    assert!(repo
        .totp_ciphertext(org_a, user_a)
        .await
        .expect("after delete")
        .is_none());

    sqlx::query("DELETE FROM mfa_recovery_codes WHERE user_id = $1")
        .bind(user_a)
        .execute(&pool)
        .await
        .expect("cleanup recovery codes");
    sqlx::query("DELETE FROM mfa_enrollments WHERE user_id = $1")
        .bind(user_a)
        .execute(&pool)
        .await
        .expect("cleanup enrollment");
    cleanup(&pool, &seed).await;
}

/// `service_account_tokens` is case 3: bearer authentication arrives with a
/// token hash and no org, so `verify` resolves the org through the
/// `SECURITY DEFINER` resolver (migration 0060) and then reads under that org's
/// context. Every other method takes the caller's org.
#[tokio::test]
#[ignore]
async fn service_account_token_repo_works_under_the_restricted_role() {
    use ione::repos::ServiceAccountTokenRepo;

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;
    let repo = ServiceAccountTokenRepo::new(restricted.clone());
    let (org_a, org_b) = (seed.org_a, seed.org_b);
    let token_hash = format!("rls-sat-{}", Uuid::new_v4());

    let token = repo
        .issue(
            org_a,
            "rls-probe",
            &token_hash,
            &serde_json::json!(["workspace:read"]),
            3,
            None,
            None,
        )
        .await
        .expect("issue must succeed under the restricted role");
    assert_eq!(token.org_id, org_a);

    let verified = repo
        .verify(&token_hash)
        .await
        .expect("verify")
        .expect("verify must find the token under the restricted role");
    assert_eq!(verified.id, token.id);
    assert_eq!(verified.org_id, org_a);
    assert!(repo
        .verify("rls-sat-unknown-hash")
        .await
        .expect("verify unknown")
        .is_none());

    let listed = repo.list_active(org_a).await.expect("list");
    assert!(listed.iter().any(|t| t.id == token.id));
    assert_eq!(
        repo.provisionable_max_coc(org_a, token.id)
            .await
            .expect("coc"),
        Some(3)
    );
    repo.touch_last_used(org_a, token.id)
        .await
        .expect("touch must succeed under the restricted role");
    let last_used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM service_account_tokens WHERE id = $1")
            .bind(token.id)
            .fetch_one(&pool)
            .await
            .expect("read last_used_at");
    assert!(last_used.is_some(), "touch under org A's context must land");

    // The other org's context sees nothing and revokes nothing.
    assert!(repo
        .list_active(org_b)
        .await
        .expect("foreign list")
        .is_empty());
    assert_eq!(
        repo.provisionable_max_coc(org_b, token.id)
            .await
            .expect("foreign coc"),
        None
    );
    assert!(!repo.revoke(token.id, org_b).await.expect("foreign revoke"));

    // The resolver hands the restricted role an org id and nothing else.
    let resolved: Option<Uuid> = sqlx::query_scalar("SELECT resolve_service_account_token_org($1)")
        .bind(&token_hash)
        .fetch_one(&restricted)
        .await
        .expect("restricted role may execute the resolver");
    assert_eq!(resolved, Some(org_a));

    assert!(repo.revoke(token.id, org_a).await.expect("revoke"));
    assert!(repo
        .verify(&token_hash)
        .await
        .expect("verify revoked")
        .is_none());

    sqlx::query("DELETE FROM service_account_tokens WHERE id = $1")
        .bind(token.id)
        .execute(&pool)
        .await
        .expect("cleanup token");
    cleanup(&pool, &seed).await;
}

/// The OAuth callback arrives with only the `state` token, so the state lookups
/// resolve the org first (`resolve_broker_credential_state_org`) and then read
/// under that org's context. `find_user_provider` takes the caller's org.
#[tokio::test]
#[ignore]
async fn broker_state_lookups_work_under_the_restricted_role() {
    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;
    let repo = ione::repos::BrokerCredentialRepo::new(restricted.clone());
    let provider = format!("{}-state", seed.provider);
    let state_token = format!("rls-state-{}", Uuid::new_v4());

    let pending = repo
        .create_pending(
            seed.user_a,
            seed.org_a,
            &provider,
            "",
            &["read".to_string()],
            &state_token,
            "verifier",
            chrono::Utc::now() + chrono::Duration::minutes(10),
        )
        .await
        .expect("create pending under the restricted role");

    let found = repo
        .find_by_state(&state_token)
        .await
        .expect("find by state")
        .expect("find_by_state must see the row under the restricted role");
    assert_eq!(found.id, pending.id);
    assert!(repo
        .find_by_state("rls-state-unknown")
        .await
        .expect("find unknown state")
        .is_none());

    // The resolver hands the restricted role an org id and nothing else.
    let resolved: Option<Uuid> =
        sqlx::query_scalar("SELECT resolve_broker_credential_state_org($1)")
            .bind(&state_token)
            .fetch_one(&restricted)
            .await
            .expect("restricted role may execute the resolver");
    assert_eq!(resolved, Some(seed.org_a));

    let consumed = repo
        .consume_by_state(&state_token)
        .await
        .expect("consume state")
        .expect("consume_by_state must see the row under the restricted role");
    assert_eq!(consumed.id, pending.id);
    assert_eq!(consumed.org_id, seed.org_a);
    assert!(consumed.state_token.is_none());
    assert!(repo
        .consume_by_state(&state_token)
        .await
        .expect("replay consume")
        .is_none());

    repo.store_tokens(pending.id, seed.org_a, b"access", None, None)
        .await
        .expect("store tokens");
    let connected = repo
        .find_user_provider(seed.user_a, &provider, seed.org_a)
        .await
        .expect("find user provider")
        .expect("find_user_provider must see the row under the restricted role");
    assert_eq!(connected.id, pending.id);

    // Org B's context cannot reach org A's connection.
    assert!(repo
        .find_user_provider(seed.user_a, &provider, seed.org_b)
        .await
        .expect("foreign find user provider")
        .is_none());

    cleanup(&pool, &seed).await;
}

/// Every `CatalogRepo` method must work under the restricted role, and another
/// org's context must never read, search, or delete org A's catalog rows.
/// Unscoped, the upsert is rejected by the policy's implicit insert check and
/// every read returns nothing, so federated search would silently go empty.
#[tokio::test]
#[ignore]
async fn catalog_repo_works_under_the_restricted_role() {
    use ione::models::CatalogEntryKind;
    use ione::repos::{CatalogRepo, CatalogUpsert};

    let pool = default_pool().await;
    let seed = seed_two_orgs(&pool).await;
    let restricted = restricted_pool().await;

    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(format!("https://issuer-{}.example", seed.provider))
    .fetch_one(&pool)
    .await
    .expect("insert trust issuer");
    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist, status)
         VALUES ($1, 'rls-catalog-peer', $3, $2, '{}'::jsonb, '[]'::jsonb,
                 'active'::peer_status)
         RETURNING id",
    )
    .bind(seed.org_a)
    .bind(issuer_id)
    .bind(format!("https://peer-{}.example/mcp", seed.provider))
    .fetch_one(&pool)
    .await
    .expect("insert peer");

    let name = "rlscat:gauge_readings".to_owned();
    let entry = |hash: &str| CatalogUpsert {
        org_id: seed.org_a,
        peer_id,
        kind: CatalogEntryKind::Tool,
        namespaced_name: name.clone(),
        raw_name: "gauge_readings".into(),
        description: "river gauge discharge readings".into(),
        sample_queries: vec!["discharge at the north gauge".into()],
        schema_field_names: vec!["discharge_cfs".into()],
        content_hash: hash.into(),
    };

    let repo = CatalogRepo::new(restricted.clone());
    assert!(repo
        .upsert_entry(&entry("h1"))
        .await
        .expect("insert must succeed under the restricted role"));
    // The conflict path has to see the existing row to update it.
    assert!(!repo
        .upsert_entry(&entry("h1"))
        .await
        .expect("unchanged upsert"));
    assert!(repo
        .upsert_entry(&entry("h2"))
        .await
        .expect("changed upsert"));

    assert_eq!(
        repo.hashes_for_peer(seed.org_a, peer_id)
            .await
            .expect("hashes"),
        [(name.clone(), "h2".to_owned())]
    );
    assert_eq!(
        repo.sample_queries_for_peer(seed.org_a, peer_id)
            .await
            .expect("sample queries"),
        [(
            name.clone(),
            vec!["discharge at the north gauge".to_owned()]
        )]
    );
    let candidates = repo
        .permission_candidates_for_org(seed.org_a)
        .await
        .expect("candidates");
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.namespaced_name.as_str())
            .collect::<Vec<_>>(),
        [name.as_str()]
    );
    let hits = repo
        .search(
            seed.org_a,
            std::slice::from_ref(&name),
            "discharge",
            None,
            10,
        )
        .await
        .expect("search");
    assert_eq!(
        hits.iter().map(|h| h.peer_id).collect::<Vec<_>>(),
        [peer_id]
    );

    // The other org's context sees nothing and deletes nothing, even when it
    // names org A's peer and entry.
    assert!(repo
        .hashes_for_peer(seed.org_b, peer_id)
        .await
        .expect("foreign hashes")
        .is_empty());
    assert!(repo
        .sample_queries_for_peer(seed.org_b, peer_id)
        .await
        .expect("foreign sample queries")
        .is_empty());
    assert!(repo
        .permission_candidates_for_org(seed.org_b)
        .await
        .expect("foreign candidates")
        .is_empty());
    assert!(repo
        .search(
            seed.org_b,
            std::slice::from_ref(&name),
            "discharge",
            None,
            10
        )
        .await
        .expect("foreign search")
        .is_empty());
    assert_eq!(
        repo.delete_orphans(seed.org_b, peer_id, &[])
            .await
            .expect("foreign delete"),
        0
    );

    assert_eq!(
        repo.delete_orphans(seed.org_a, peer_id, &[])
            .await
            .expect("delete"),
        1
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM peer_catalog_entries WHERE peer_id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("count remaining");
    assert_eq!(remaining, 0);

    sqlx::query("DELETE FROM peers WHERE id = $1")
        .bind(peer_id)
        .execute(&pool)
        .await
        .expect("cleanup peer");
    sqlx::query("DELETE FROM trust_issuers WHERE id = $1")
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("cleanup issuer");
    cleanup(&pool, &seed).await;
}

async fn cleanup(pool: &PgPool, seed: &TwoOrgs) {
    sqlx::query("DELETE FROM broker_credentials WHERE org_id = ANY($1)")
        .bind(vec![seed.org_a, seed.org_b])
        .execute(pool)
        .await
        .expect("delete seeded credentials");
    sqlx::query("DELETE FROM users WHERE org_id = ANY($1)")
        .bind(vec![seed.org_a, seed.org_b])
        .execute(pool)
        .await
        .expect("delete seeded users");
    sqlx::query("DELETE FROM organizations WHERE id = ANY($1)")
        .bind(vec![seed.org_a, seed.org_b])
        .execute(pool)
        .await
        .expect("delete seeded organizations");
}
