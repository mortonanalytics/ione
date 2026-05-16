use serde::Deserialize;
use serde_json::Value;
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

    pub async fn exchange_code_for_claims(
        &self,
        ti: &TrustIssuer,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
        expected_nonce: &str,
    ) -> anyhow::Result<Value> {
        let discovery = discover(self.http, ti).await?;
        let client_id = ti.client_id.as_deref().unwrap_or(&ti.audience);
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("code_verifier", code_verifier.to_string()),
            ("client_id", client_id.to_string()),
            ("redirect_uri", redirect_uri.to_string()),
        ];
        if let Some(ciphertext) = ti.client_secret_ciphertext.as_deref() {
            let secret = crate::util::token_crypto::decrypt_versioned(ciphertext)?;
            form.push((
                "client_secret",
                String::from_utf8(secret)
                    .map_err(|_| anyhow::anyhow!("client secret is not valid utf-8"))?,
            ));
        }
        let token: TokenResponse = self
            .http
            .post(&discovery.token_endpoint)
            .form(&form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let id_token = token
            .id_token
            .ok_or_else(|| anyhow::anyhow!("token response missing id_token"))?;
        validate_id_token(
            self.http,
            ti,
            &discovery.jwks_uri,
            &id_token,
            expected_nonce,
        )
        .await
    }
}

#[derive(Debug, Deserialize)]
struct OpenIdConfiguration {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

async fn discover(http: &reqwest::Client, ti: &TrustIssuer) -> anyhow::Result<OpenIdConfiguration> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        ti.issuer_url.trim_end_matches('/')
    );
    http.get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(Into::into)
}

async fn discovery_authorization_endpoint(
    http: &reqwest::Client,
    ti: &TrustIssuer,
) -> anyhow::Result<String> {
    Ok(discover(http, ti).await?.authorization_endpoint)
}

async fn validate_id_token(
    http: &reqwest::Client,
    ti: &TrustIssuer,
    jwks_uri: &str,
    id_token: &str,
    expected_nonce: &str,
) -> anyhow::Result<Value> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};

    let header = decode_header(id_token)?;
    let alg = header.alg;
    let kid = header.kid.as_deref();
    let jwks: jsonwebtoken::jwk::JwkSet = http
        .get(if ti.jwks_uri.is_empty() {
            jwks_uri
        } else {
            &ti.jwks_uri
        })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let jwk = jwks
        .keys
        .iter()
        .find(|key| {
            kid.is_none()
                || key
                    .common
                    .key_id
                    .as_deref()
                    .map(|key_id| Some(key_id) == kid)
                    .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("no matching jwk for id_token"))?;
    let key = DecodingKey::from_jwk(jwk)?;
    let algorithm = match alg {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::ES256 => alg,
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => alg,
        other => anyhow::bail!("unsupported id_token algorithm: {:?}", other),
    };
    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[ti.issuer_url.as_str()]);
    validation.set_audience(&[ti.audience.as_str()]);
    let token = decode::<Value>(id_token, &key, &validation)?;
    if token.claims["nonce"].as_str() != Some(expected_nonce) {
        anyhow::bail!("id_token nonce mismatch");
    }
    Ok(token.claims)
}
