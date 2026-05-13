use anyhow::Context;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::TrustIssuer;

pub struct TrustIssuerRepo {
    pub pool: PgPool,
}

impl TrustIssuerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        org_id: Uuid,
        issuer_url: &str,
        audience: &str,
        jwks_uri: &str,
        claim_mapping: Value,
    ) -> anyhow::Result<TrustIssuer> {
        sqlx::query_as::<_, TrustIssuer>(
            "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, org_id, issuer_url, audience, jwks_uri, claim_mapping,
                idp_type, max_coc_level, client_id, client_secret_ciphertext, display_name",
        )
        .bind(org_id)
        .bind(issuer_url)
        .bind(audience)
        .bind(jwks_uri)
        .bind(claim_mapping)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert trust_issuer")
    }

    pub async fn find_by_issuer_url(
        &self,
        org_id: Uuid,
        issuer_url: &str,
    ) -> anyhow::Result<Option<TrustIssuer>> {
        sqlx::query_as::<_, TrustIssuer>(
            "SELECT id, org_id, issuer_url, audience, jwks_uri, claim_mapping,
                idp_type, max_coc_level, client_id, client_secret_ciphertext, display_name
             FROM trust_issuers
             WHERE org_id = $1 AND issuer_url = $2
             LIMIT 1",
        )
        .bind(org_id)
        .bind(issuer_url)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find trust_issuer by issuer_url")
    }

    pub async fn list(&self, org_id: Uuid) -> anyhow::Result<Vec<TrustIssuer>> {
        sqlx::query_as::<_, TrustIssuer>(
            "SELECT id, org_id, issuer_url, audience, jwks_uri, claim_mapping,
                idp_type, max_coc_level, client_id, client_secret_ciphertext, display_name
             FROM trust_issuers
             WHERE org_id = $1
             ORDER BY issuer_url ASC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list trust_issuers")
    }

    pub async fn find_by_id(&self, org_id: Uuid, id: Uuid) -> anyhow::Result<Option<TrustIssuer>> {
        sqlx::query_as::<_, TrustIssuer>(
            "SELECT id, org_id, issuer_url, audience, jwks_uri, claim_mapping,
                idp_type, max_coc_level, client_id, client_secret_ciphertext, display_name
             FROM trust_issuers
             WHERE org_id = $1 AND id = $2",
        )
        .bind(org_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find trust_issuer by id")
    }

    pub async fn create_oidc(
        &self,
        org_id: Uuid,
        issuer_url: &str,
        audience: &str,
        jwks_uri: &str,
        claim_mapping: Value,
        max_coc_level: i32,
        client_secret_ciphertext: Option<Vec<u8>>,
        display_name: Option<String>,
    ) -> anyhow::Result<TrustIssuer> {
        sqlx::query_as::<_, TrustIssuer>(
            "INSERT INTO trust_issuers
                (org_id, issuer_url, audience, jwks_uri, claim_mapping, idp_type,
                 max_coc_level, client_id, client_secret_ciphertext, display_name)
             VALUES ($1, $2, $3, $4, $5, 'oidc', $6, $7, $8, $9)
             RETURNING id, org_id, issuer_url, audience, jwks_uri, claim_mapping,
                idp_type, max_coc_level, client_id, client_secret_ciphertext, display_name",
        )
        .bind(org_id)
        .bind(issuer_url)
        .bind(audience)
        .bind(jwks_uri)
        .bind(claim_mapping)
        .bind(max_coc_level)
        .bind(audience)
        .bind(client_secret_ciphertext)
        .bind(display_name)
        .fetch_one(&self.pool)
        .await
        .context("failed to create oidc trust issuer")
    }

    pub async fn delete(&self, org_id: Uuid, id: Uuid) -> anyhow::Result<u64> {
        let rows = sqlx::query("DELETE FROM trust_issuers WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete trust issuer")?
            .rows_affected();
        Ok(rows)
    }
}
