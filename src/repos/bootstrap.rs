use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

/// Idempotently create the default organization ("Default Org") and default user
/// ("default@localhost"). Returns (org_id, user_id).
pub async fn ensure_default_org_and_user(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid)> {
    let org_id: Uuid = sqlx::query_scalar(
        "INSERT INTO organizations (name)
         VALUES ('Default Org')
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .fetch_optional(pool)
    .await
    .context("failed to upsert default org")?
    .unwrap_or_else(|| {
        // Row already existed; fetch it.
        // This branch executes synchronously; we re-query below if needed.
        Uuid::nil()
    });

    // If the INSERT returned nothing (already existed), look it up.
    let org_id = if org_id == Uuid::nil() {
        sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
            .fetch_one(pool)
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
    .fetch_optional(pool)
    .await
    .context("failed to upsert default user")?
    .unwrap_or_else(Uuid::nil);

    let user_id = if user_id == Uuid::nil() {
        sqlx::query_scalar(
            "SELECT id FROM users WHERE org_id = $1 AND email = 'default@localhost' LIMIT 1",
        )
        .bind(org_id)
        .fetch_one(pool)
        .await
        .context("failed to fetch default user")?
    } else {
        user_id
    };

    Ok((org_id, user_id))
}
