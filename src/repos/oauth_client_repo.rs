use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::OauthClient;

pub struct OauthClientRepo {
    pool: PgPool,
}

impl OauthClientRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn register(
        &self,
        client_id: &str,
        client_metadata: &serde_json::Value,
        display_name: &str,
        registered_by_user_id: Option<Uuid>,
    ) -> Result<OauthClient> {
        let row = sqlx::query_as::<_, OauthClient>(
            "INSERT INTO oauth_clients (client_id, client_metadata, display_name, registered_by_user_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (client_id) DO UPDATE SET client_metadata = EXCLUDED.client_metadata, display_name = EXCLUDED.display_name
             RETURNING id, client_id, client_metadata, registered_by_user_id, display_name, created_at, last_seen_at",
        )
        .bind(client_id)
        .bind(client_metadata)
        .bind(display_name)
        .bind(registered_by_user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_by_client_id(&self, client_id: &str) -> Result<Option<OauthClient>> {
        let row = sqlx::query_as::<_, OauthClient>(
            "SELECT id, client_id, client_metadata, registered_by_user_id, display_name, created_at, last_seen_at
               FROM oauth_clients WHERE client_id = $1",
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn touch_last_seen(&self, client_id: &str) -> Result<()> {
        sqlx::query("UPDATE oauth_clients SET last_seen_at = now() WHERE client_id = $1")
            .bind(client_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<OauthClient>> {
        let rows = sqlx::query_as::<_, OauthClient>(
            "SELECT DISTINCT c.id, c.client_id, c.client_metadata, c.registered_by_user_id, c.display_name, c.created_at, c.last_seen_at
               FROM oauth_clients c
               JOIN oauth_access_tokens t ON t.client_id = c.client_id
              WHERE t.user_id = $1 AND t.revoked_at IS NULL
              ORDER BY c.last_seen_at DESC NULLS LAST",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
