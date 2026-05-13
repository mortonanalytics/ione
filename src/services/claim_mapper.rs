use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    models::{TrustIssuer, User},
    repos::{MembershipRepo, RoleRepo, UserRepo},
};

pub struct ClaimMapper;

impl ClaimMapper {
    pub async fn map_to_user(
        pool: &PgPool,
        org_id: Uuid,
        trust_issuer: &TrustIssuer,
        claims: &Value,
    ) -> anyhow::Result<User> {
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

        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM federated_identities WHERE issuer_id = $1 AND subject = $2",
        )
        .bind(trust_issuer.id)
        .bind(subject)
        .fetch_optional(pool)
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
            .execute(pool)
            .await?;
            return UserRepo::new(pool.clone())
                .get(existing)
                .await?
                .ok_or_else(|| anyhow::anyhow!("federated user not found"));
        }

        let user = UserRepo::new(pool.clone())
            .insert_profile(org_id, email, display_name, Some(subject))
            .await?;
        sqlx::query(
            "INSERT INTO federated_identities (issuer_id, subject, user_id, last_seen_email)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(trust_issuer.id)
        .bind(subject)
        .bind(user.id)
        .bind(email)
        .execute(pool)
        .await?;

        let role_name = claims[role_claim].as_str().unwrap_or("member");
        let raw_coc = claims[coc_level_claim].as_i64().unwrap_or(0) as i32;
        let coc_level = raw_coc.clamp(0, trust_issuer.max_coc_level);
        let workspace_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM workspaces WHERE org_id = $1 ORDER BY created_at LIMIT 1",
        )
        .bind(org_id)
        .fetch_one(pool)
        .await?;
        let role = RoleRepo::new(pool.clone())
            .upsert(workspace_id, role_name, coc_level)
            .await?;
        MembershipRepo::new(pool.clone())
            .upsert_federated(
                user.id,
                workspace_id,
                role.id,
                &format!("{}@{}", subject, trust_issuer.issuer_url),
            )
            .await?;
        Ok(user)
    }
}
