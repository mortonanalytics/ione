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
