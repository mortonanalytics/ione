use anyhow::Result;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{OauthAccessToken, OauthAuthCode, OauthRefreshToken};

pub struct OauthTokenRepo {
    pool: PgPool,
}

impl OauthTokenRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_auth_code(
        &self,
        code: &str,
        client_id: &str,
        user_id: Uuid,
        redirect_uri: &str,
        scope: &str,
        code_challenge: &str,
        code_challenge_method: &str,
        ttl_seconds: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_auth_codes (code, client_id, user_id, redirect_uri, scope, code_challenge, code_challenge_method, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(code)
        .bind(client_id)
        .bind(user_id)
        .bind(redirect_uri)
        .bind(scope)
        .bind(code_challenge)
        .bind(code_challenge_method)
        .bind(Utc::now() + Duration::seconds(ttl_seconds))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn consume_auth_code(&self, code: &str) -> Result<Option<OauthAuthCode>> {
        let row = sqlx::query_as::<_, OauthAuthCode>(
            "UPDATE oauth_auth_codes
                SET consumed_at = now()
              WHERE code = $1 AND consumed_at IS NULL AND expires_at > now()
          RETURNING code, client_id, user_id, redirect_uri, scope, code_challenge, code_challenge_method, expires_at, consumed_at",
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn insert_access_token(
        &self,
        token_hash: &str,
        client_id: &str,
        user_id: Uuid,
        scope: &str,
        ttl_seconds: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_access_tokens (token_hash, client_id, user_id, scope, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(token_hash)
        .bind(client_id)
        .bind(user_id)
        .bind(scope)
        .bind(Utc::now() + Duration::seconds(ttl_seconds))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_access_token(&self, token_hash: &str) -> Result<Option<OauthAccessToken>> {
        let row = sqlx::query_as::<_, OauthAccessToken>(
            "SELECT token_hash, client_id, user_id, scope, expires_at, created_at, revoked_at
               FROM oauth_access_tokens
              WHERE token_hash = $1
                AND revoked_at IS NULL
                AND expires_at > now()",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn revoke_client_tokens(&self, client_id: &str, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE oauth_access_tokens SET revoked_at = now() WHERE client_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(client_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE oauth_refresh_tokens SET revoked_at = now() WHERE client_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(client_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_refresh_token(
        &self,
        token_hash: &str,
        client_id: &str,
        user_id: Uuid,
        scope: &str,
        ttl_seconds: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_refresh_tokens (token_hash, client_id, user_id, scope, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(token_hash)
        .bind(client_id)
        .bind(user_id)
        .bind(scope)
        .bind(Utc::now() + Duration::seconds(ttl_seconds))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn consume_refresh_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<OauthRefreshToken>> {
        let row = sqlx::query_as::<_, OauthRefreshToken>(
            "UPDATE oauth_refresh_tokens
                SET revoked_at = now()
              WHERE token_hash = $1
                AND revoked_at IS NULL
                AND expires_at > now()
          RETURNING token_hash, client_id, user_id, scope, expires_at, created_at, revoked_at",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }
}
