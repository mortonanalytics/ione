use crate::models::dataset_delegation::DatasetDelegation;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub struct DatasetDelegationRepo(pub PgPool);

impl DatasetDelegationRepo {
    pub async fn insert(&self, grant: &DatasetDelegation, token_hash: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO dataset_delegations (id,org_id,workspace_id,owner_user_id,dataset_id,version_id,token_hash,manifest,expires_at,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(grant.grant_id).bind(grant.owner.tenant_id).bind(grant.owner.workspace_id)
            .bind(Uuid::parse_str(&grant.owner.actor_id)?).bind(grant.dataset_id).bind(grant.version_id)
            .bind(token_hash).bind(serde_json::to_value(grant)?).bind(grant.expires_at).bind(grant.created_at)
            .execute(&self.0).await?;
        Ok(())
    }

    pub async fn authenticate(
        &self,
        id: Uuid,
        hash: &str,
    ) -> anyhow::Result<Option<DatasetDelegation>> {
        let value: Option<serde_json::Value> = sqlx::query_scalar("SELECT manifest FROM dataset_delegations WHERE id=$1 AND token_hash=$2 AND revoked_at IS NULL AND expires_at > now()")
            .bind(id).bind(hash).fetch_optional(&self.0).await?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    pub async fn list(
        &self,
        owner: &crate::models::dataset_delegation::DatasetIdentity,
        dataset: Uuid,
        version: Uuid,
    ) -> anyhow::Result<Vec<DatasetDelegation>> {
        let rows: Vec<(serde_json::Value, Option<chrono::DateTime<Utc>>)> = sqlx::query_as("SELECT manifest,revoked_at FROM dataset_delegations WHERE org_id=$1 AND workspace_id=$2 AND owner_user_id=$3 AND dataset_id=$4 AND version_id=$5 ORDER BY created_at DESC LIMIT 100")
            .bind(owner.tenant_id).bind(owner.workspace_id).bind(Uuid::parse_str(&owner.actor_id)?).bind(dataset).bind(version).fetch_all(&self.0).await?;
        rows.into_iter()
            .map(|(value, revoked_at)| {
                let mut grant: DatasetDelegation = serde_json::from_value(value)?;
                grant.revoked_at = revoked_at;
                Ok(grant)
            })
            .collect()
    }

    pub async fn revoke(
        &self,
        owner: &crate::models::dataset_delegation::DatasetIdentity,
        dataset: Uuid,
        version: Uuid,
        id: Uuid,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query("UPDATE dataset_delegations SET revoked_at=COALESCE(revoked_at,now()) WHERE id=$1 AND org_id=$2 AND workspace_id=$3 AND owner_user_id=$4 AND dataset_id=$5 AND version_id=$6 AND manifest->'owner'=$7")
            .bind(id).bind(owner.tenant_id).bind(owner.workspace_id).bind(Uuid::parse_str(&owner.actor_id)?).bind(dataset).bind(version).bind(serde_json::to_value(owner)?)
            .execute(&self.0).await?.rows_affected() == 1)
    }
}
