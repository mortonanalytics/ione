use sqlx::PgPool;
use uuid::Uuid;

pub struct BrokeredTokenService<'a> {
    pool: &'a PgPool,
}

impl<'a> BrokeredTokenService<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_for_user(
        &self,
        user_id: Uuid,
        provider: &str,
    ) -> anyhow::Result<Option<String>> {
        let row = crate::repos::BrokerCredentialRepo::new(self.pool.clone())
            .find_user_provider(user_id, provider)
            .await?;
        row.and_then(|cred| cred.access_token_ciphertext)
            .map(|cipher| crate::util::token_crypto::decrypt_versioned(&cipher))
            .transpose()
            .map(|opt| opt.map(|bytes| String::from_utf8_lossy(&bytes).to_string()))
    }
}
