use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::User;

pub struct UserRepo {
    pub pool: PgPool,
}

impl UserRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<User>> {
        sqlx::query_as::<_, User>(
            "SELECT id, org_id, email, display_name, oidc_subject, created_at
             FROM users
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get user by id")
    }

    /// Upsert a user by (org_id, oidc_subject). If a user with this oidc_subject
    /// already exists in the org, update email and display_name; otherwise insert.
    /// Returns the upserted User row.
    pub async fn upsert_by_oidc_subject(
        &self,
        org_id: Uuid,
        email: &str,
        display_name: &str,
        oidc_subject: &str,
    ) -> anyhow::Result<User> {
        sqlx::query_as::<_, User>(
            "INSERT INTO users (org_id, email, display_name, oidc_subject)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (org_id, email) DO UPDATE
               SET display_name  = EXCLUDED.display_name,
                   oidc_subject  = EXCLUDED.oidc_subject
             RETURNING id, org_id, email, display_name, oidc_subject, created_at",
        )
        .bind(org_id)
        .bind(email)
        .bind(display_name)
        .bind(oidc_subject)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert user by oidc_subject")
    }
}
