use sqlx::PgPool;

use crate::models::TrustIssuer;

pub struct IdpService<'a> {
    #[allow(dead_code)]
    pool: &'a PgPool,
    http: &'a reqwest::Client,
}

impl<'a> IdpService<'a> {
    pub fn new(pool: &'a PgPool, http: &'a reqwest::Client) -> Self {
        Self { pool, http }
    }

    pub async fn authorize_url(
        &self,
        ti: &TrustIssuer,
        redirect_uri: &str,
    ) -> anyhow::Result<(String, String, String)> {
        let nonce = crate::auth::random_url_safe_string();
        let verifier = crate::auth::random_url_safe_string();
        let challenge = crate::auth::pkce_challenge(&verifier);
        let endpoint = discovery_authorization_endpoint(self.http, ti).await?;
        let url = format!(
            "{}?client_id={}&response_type=code&redirect_uri={}&scope=openid%20email%20profile&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
            endpoint,
            urlencoding::encode(&ti.audience),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(&nonce),
            urlencoding::encode(&nonce),
            urlencoding::encode(&challenge)
        );
        Ok((url, nonce, verifier))
    }
}

async fn discovery_authorization_endpoint(
    http: &reqwest::Client,
    ti: &TrustIssuer,
) -> anyhow::Result<String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        ti.issuer_url.trim_end_matches('/')
    );
    let doc: serde_json::Value = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    doc["authorization_endpoint"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("openid configuration missing authorization_endpoint"))
}
