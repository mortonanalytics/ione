use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{TrustIssuer, User};

pub struct ClaimMapper;

impl ClaimMapper {
    pub async fn map_to_user(
        pool: &PgPool,
        org_id: Uuid,
        trust_issuer: &TrustIssuer,
        claims: &Value,
    ) -> anyhow::Result<User> {
        let mut tx = pool.begin().await?;
        let subject = claims["sub"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("claims missing sub"))?;
        let mapping = &trust_issuer.claim_mapping;
        let email_claim = mapping["email_claim"].as_str().unwrap_or("email");
        let name_claim = mapping["name_claim"].as_str().unwrap_or("name");
        let role_claim = mapping["role_claim"].as_str().unwrap_or("ione_role");
        let coc_level_claim = mapping["coc_level_claim"]
            .as_str()
            .unwrap_or("ione_coc_level");
        let email = claims[email_claim]
            .as_str()
            .or_else(|| claims["preferred_username"].as_str())
            .unwrap_or(subject);
        let display_name = claims[name_claim].as_str().unwrap_or(email);

        let workspace_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM workspaces WHERE org_id = $1 ORDER BY created_at LIMIT 1",
        )
        .bind(org_id)
        .fetch_one(&mut *tx)
        .await?;

        let role_name = claims[role_claim].as_str().unwrap_or("member");
        let raw_coc = claims[coc_level_claim].as_i64().unwrap_or(0) as i32;
        let coc_level = raw_coc.clamp(0, trust_issuer.max_coc_level);
        let role_id: Uuid = sqlx::query_scalar(
            "INSERT INTO roles (workspace_id, name, coc_level)
             VALUES ($1, $2, $3)
             ON CONFLICT (workspace_id, name, coc_level) DO UPDATE
               SET coc_level = EXCLUDED.coc_level
             RETURNING id",
        )
        .bind(workspace_id)
        .bind(role_name)
        .bind(coc_level)
        .fetch_one(&mut *tx)
        .await?;

        let user = if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM federated_identities WHERE issuer_id = $1 AND subject = $2",
        )
        .bind(trust_issuer.id)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await?
        {
            sqlx::query(
                "UPDATE federated_identities
                 SET last_seen_email = $3, updated_at = now()
                 WHERE issuer_id = $1 AND subject = $2",
            )
            .bind(trust_issuer.id)
            .bind(subject)
            .bind(email)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE users
                 SET email = $2, display_name = $3, oidc_subject = $4
                 WHERE id = $1",
            )
            .bind(existing)
            .bind(email)
            .bind(display_name)
            .bind(subject)
            .execute(&mut *tx)
            .await?;
            sqlx::query_as::<_, User>(
                "SELECT id, org_id, email, display_name, oidc_subject, created_at
                 FROM users
                 WHERE id = $1",
            )
            .bind(existing)
            .fetch_one(&mut *tx)
            .await?
        } else {
            let user = sqlx::query_as::<_, User>(
                "INSERT INTO users (org_id, email, display_name, oidc_subject)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (org_id, email) DO UPDATE
                   SET display_name = EXCLUDED.display_name,
                       oidc_subject = EXCLUDED.oidc_subject
                 RETURNING id, org_id, email, display_name, oidc_subject, created_at",
            )
            .bind(org_id)
            .bind(email)
            .bind(display_name)
            .bind(subject)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO federated_identities (issuer_id, subject, user_id, last_seen_email)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (user_id, issuer_id) DO UPDATE
                   SET subject = EXCLUDED.subject,
                       last_seen_email = EXCLUDED.last_seen_email,
                       updated_at = now()",
            )
            .bind(trust_issuer.id)
            .bind(subject)
            .bind(user.id)
            .bind(email)
            .execute(&mut *tx)
            .await?;
            user
        };

        sqlx::query(
            "INSERT INTO memberships (user_id, workspace_id, role_id, federated_claim_ref)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id, workspace_id, role_id) DO UPDATE
               SET federated_claim_ref = EXCLUDED.federated_claim_ref",
        )
        .bind(user.id)
        .bind(workspace_id)
        .bind(role_id)
        .bind(format!("{}@{}", subject, trust_issuer.issuer_url))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(user)
    }
}
