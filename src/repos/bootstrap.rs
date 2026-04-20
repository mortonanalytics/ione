use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

/// Idempotently create the default organization ("Default Org") and default user
/// ("default@localhost"). Returns (org_id, user_id).
///
/// Wrapped in a transaction with pg_advisory_xact_lock to serialize concurrent
/// bootstrap calls (e.g. test parallelism), closing the Phase 2 known-followup.
pub async fn ensure_default_org_and_user(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid)> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin bootstrap transaction")?;

    // Serialize concurrent bootstrap calls across processes/tests.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ione_bootstrap'))")
        .execute(&mut *tx)
        .await
        .context("failed to acquire bootstrap advisory lock")?;

    let org_id: Uuid = sqlx::query_scalar(
        "INSERT INTO organizations (name)
         VALUES ('Default Org')
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .fetch_optional(&mut *tx)
    .await
    .context("failed to upsert default org")?
    .unwrap_or(Uuid::nil());

    let org_id = if org_id == Uuid::nil() {
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
            .fetch_one(&mut *tx)
            .await
            .context("failed to fetch default org")?
    } else {
        org_id
    };

    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (org_id, email, display_name)
         VALUES ($1, 'default@localhost', 'Default User')
         ON CONFLICT (org_id, email) DO NOTHING
         RETURNING id",
    )
    .bind(org_id)
    .fetch_optional(&mut *tx)
    .await
    .context("failed to upsert default user")?
    .unwrap_or(Uuid::nil());

    let user_id = if user_id == Uuid::nil() {
        sqlx::query_scalar(
            "SELECT id FROM users WHERE org_id = $1 AND email = 'default@localhost' LIMIT 1",
        )
        .bind(org_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to fetch default user")?
    } else {
        user_id
    };

    tx.commit()
        .await
        .context("failed to commit bootstrap transaction")?;

    Ok((org_id, user_id))
}

/// Idempotently ensure the "Operations" workspace, a "member" role scoped to it,
/// and a membership for `user_id` in that workspace+role.
///
/// Returns the Operations workspace id.
///
/// Wrapped in a transaction with pg_advisory_xact_lock to serialize concurrent
/// calls (e.g. test parallelism), matching the approach used in
/// `ensure_default_org_and_user`.
pub async fn ensure_default_workspace_and_membership(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<Uuid> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin workspace bootstrap transaction")?;

    // Same advisory lock key as org/user bootstrap — serializes all bootstrap phases.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ione_bootstrap'))")
        .execute(&mut *tx)
        .await
        .context("failed to acquire workspace bootstrap advisory lock")?;

    // Ensure Operations workspace
    let ops_ws_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM workspaces WHERE org_id = $1 AND name = 'Operations' LIMIT 1",
    )
    .bind(org_id)
    .fetch_optional(&mut *tx)
    .await
    .context("failed to query Operations workspace")?
    .unwrap_or(Uuid::nil());

    let ops_ws_id = if ops_ws_id == Uuid::nil() {
        sqlx::query_scalar(
            "INSERT INTO workspaces (org_id, name, domain, lifecycle)
             VALUES ($1, 'Operations', 'generic', 'continuous')
             RETURNING id",
        )
        .bind(org_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to insert Operations workspace")?
    } else {
        ops_ws_id
    };

    // Ensure "member" role on Operations workspace
    let member_role_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM roles WHERE workspace_id = $1 AND name = 'member' LIMIT 1",
    )
    .bind(ops_ws_id)
    .fetch_optional(&mut *tx)
    .await
    .context("failed to query member role")?
    .unwrap_or(Uuid::nil());

    let member_role_id = if member_role_id == Uuid::nil() {
        sqlx::query_scalar(
            "INSERT INTO roles (workspace_id, name, coc_level)
             VALUES ($1, 'member', 0)
             RETURNING id",
        )
        .bind(ops_ws_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to insert member role")?
    } else {
        member_role_id
    };

    // Ensure membership: user × Operations × member
    sqlx::query(
        "INSERT INTO memberships (user_id, workspace_id, role_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id, workspace_id, role_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(ops_ws_id)
    .bind(member_role_id)
    .execute(&mut *tx)
    .await
    .context("failed to upsert default membership")?;

    tx.commit()
        .await
        .context("failed to commit workspace bootstrap transaction")?;

    Ok(ops_ws_id)
}
