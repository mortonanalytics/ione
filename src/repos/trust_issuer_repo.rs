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
             RETURNING id, org_id, issuer_url, audience, jwks_uri, claim_mapping",
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
            "SELECT id, org_id, issuer_url, audience, jwks_uri, claim_mapping
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
            "SELECT id, org_id, issuer_url, audience, jwks_uri, claim_mapping
             FROM trust_issuers
             WHERE org_id = $1
             ORDER BY issuer_url ASC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list trust_issuers")
    }
}
