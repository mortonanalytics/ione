//! Issue #26 — the `peers` projection contract.
//!
//! `Peer` was hydrated by three hand-written column lists (`PeerRepo`, the
//! `workspace_peer_bindings` join, and `federation::peer_by_prefix`). A list
//! that missed `tool_allowlist_configured` did not error, because the field
//! carried `#[sqlx(default)]` — it read as `false`, which `tool_is_allowlisted`
//! interprets as "no allowlist configured" and lets every tool through.
//! A security gate turned off by an omission three layers away.
//!
//! Run (serial, ignored):
//!   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//!     IONE_SKIP_LIVE=1 \
//!     cargo test --test peer_projection_contract_integration -- --ignored --test-threads=1

use ione::models::Peer;
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn setup() -> PgPool {
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
        "TRUNCATE workspace_peer_bindings, peers, trust_issuers, workspaces, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    pool
}

/// Seed an org, a workspace, an issuer, and one active peer whose allowlist has
/// been configured, bound to the workspace. Returns `(workspace_id, peer_id)`.
async fn seed_configured_peer(pool: &PgPool) -> (Uuid, Uuid) {
    let org_id: Uuid =
        sqlx::query_scalar("INSERT INTO organizations (name) VALUES ('proj-org') RETURNING id")
            .fetch_one(pool)
            .await
            .expect("insert org");

    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, 'proj-ws', 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'https://issuer.example', 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("insert trust issuer");

    let peer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO peers
           (org_id, name, mcp_url, issuer_id, sharing_policy, tool_allowlist,
            tool_allowlist_configured, tool_prefix, status)
         VALUES ($1, 'proj-peer', 'https://peer.example/mcp', $2, '{}'::jsonb,
                 '[\"allowed_tool\"]'::jsonb, true, 'projpeer', 'active'::peer_status)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("insert peer");

    sqlx::query(
        "INSERT INTO workspace_peer_bindings
           (workspace_id, peer_id, foreign_tenant_id, foreign_workspace_id, status)
         VALUES ($1, $2, 'tenant-a', 'remote-ws', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(pool)
    .await
    .expect("insert binding");

    (workspace_id, peer_id)
}

/// The contract: a projection that omits a real `peers` column must fail at the
/// query, not hand back a type default.
///
/// This is the test that fails before the fix. With `#[sqlx(default)]` on
/// `tool_allowlist_configured`, the query below succeeds and reports `false`
/// for a row whose stored value is `true` — the exact shape that silently
/// disables the allowlist gate.
#[tokio::test]
#[ignore]
async fn omitting_a_column_errors_instead_of_defaulting() {
    let pool = setup().await;
    let (_ws, peer_id) = seed_configured_peer(&pool).await;

    let projection_missing_the_gate = Peer::COLUMNS.replace("tool_allowlist_configured,", "");
    assert!(
        !projection_missing_the_gate.contains("tool_allowlist_configured"),
        "the column must actually be gone from the projection under test"
    );

    let result = sqlx::query_as::<_, Peer>(&format!(
        "SELECT {projection_missing_the_gate} FROM peers WHERE id = $1"
    ))
    .bind(peer_id)
    .fetch_one(&pool)
    .await;

    match result {
        Ok(peer) => panic!(
            "a projection missing `tool_allowlist_configured` hydrated anyway and read as {} \
             while the stored value is true — the allowlist gate is off",
            peer.tool_allowlist_configured
        ),
        Err(sqlx::Error::ColumnNotFound(column)) => {
            assert_eq!(column, "tool_allowlist_configured");
        }
        Err(other) => panic!("expected ColumnNotFound, got {other:?}"),
    }
}

/// Every projection renders from `Peer::COLUMNS`, so every projection carries
/// the gate. Checked through the two public paths.
#[tokio::test]
#[ignore]
async fn every_projection_hydrates_the_allowlist_gate() {
    let pool = setup().await;
    let (workspace_id, peer_id) = seed_configured_peer(&pool).await;

    let org_id: Uuid = sqlx::query_scalar("SELECT org_id FROM peers WHERE id = $1")
        .bind(peer_id)
        .fetch_one(&pool)
        .await
        .expect("peer org");

    let by_id = ione::repos::PeerRepo::new(pool.clone())
        .get(peer_id)
        .await
        .expect("get peer")
        .expect("peer exists");
    assert!(
        by_id.tool_allowlist_configured,
        "PeerRepo projection dropped the allowlist gate"
    );

    let for_workspace = ione::repos::WorkspacePeerBindingRepo::new(pool.clone())
        .list_active_peers_for_workspace(workspace_id, org_id)
        .await
        .expect("list active peers");
    assert_eq!(for_workspace.len(), 1, "the bound peer should be listed");
    assert!(
        for_workspace[0].tool_allowlist_configured,
        "the workspace-binding join dropped the allowlist gate"
    );
}

/// `webhook_secret_ciphertext` is deliberately not in `Peer::COLUMNS` — it is
/// read only by `PeerRepo::get_with_webhook_secret`, which asks for it by name.
/// If a future change adds it to the shared list, this fails and says why.
#[test]
fn the_secret_column_stays_out_of_the_shared_projection() {
    for column in Peer::NON_PROJECTED_COLUMNS {
        assert!(
            !Peer::COLUMNS.contains(column),
            "`{column}` must not be in the shared projection: it is read only where it is needed"
        );
    }
}
