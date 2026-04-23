use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Form, Json,
};
use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{auth::AuthContext, error::AppError, models::ClientMetadata, state::AppState};

const ACCESS_TOKEN_TTL_SECS: i64 = 3_600;
const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;
const AUTH_CODE_TTL_SECS: i64 = 600;

pub(crate) async fn discovery(State(state): State<AppState>) -> Json<Value> {
    let issuer = state.config.oauth_issuer.clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/mcp/oauth/authorize"),
        "token_endpoint": format!("{issuer}/mcp/oauth/token"),
        "registration_endpoint": format!("{issuer}/mcp/oauth/register"),
        "revocation_endpoint": format!("{issuer}/mcp/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "client_id_metadata_document_supported": true
    }))
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum RegisterBody {
    Cimd {
        #[serde(alias = "clientMetadataUrl")]
        client_metadata_url: String,
    },
    Direct(ClientMetadata),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegisterResp {
    pub client_id: String,
}

pub(crate) async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<RegisterResp>, AppError> {
    let metadata_json: Value = match body {
        RegisterBody::Cimd {
            client_metadata_url,
        } => {
            let resp = reqwest::Client::new()
                .get(&client_metadata_url)
                .send()
                .await
                .map_err(|e| AppError::BadRequest(format!("CIMD fetch failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(AppError::BadRequest(format!(
                    "CIMD fetch returned {}",
                    resp.status()
                )));
            }
            resp.json()
                .await
                .map_err(|e| AppError::BadRequest(format!("CIMD parse: {e}")))?
        }
        RegisterBody::Direct(m) => serde_json::to_value(&m)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize client metadata: {e}")))?,
    };

    let parsed: ClientMetadata = serde_json::from_value(metadata_json.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid client metadata: {e}")))?;
    let display_name = parsed
        .client_name
        .clone()
        .unwrap_or_else(|| "unknown client".to_string());

    let client_id = format!("ione-client-{}", Uuid::new_v4());
    let repo = crate::repos::OauthClientRepo::new(state.pool.clone());
    repo.register(&client_id, &metadata_json, &display_name, None)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(RegisterResp { client_id }))
}

#[derive(Deserialize)]
pub(crate) struct AuthorizeQuery {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    #[serde(default = "default_challenge_method")]
    pub code_challenge_method: String,
    pub scope: Option<String>,
    pub state: Option<String>,
    #[serde(default = "default_response_type")]
    pub response_type: String,
}

fn default_challenge_method() -> String {
    "S256".into()
}

fn default_response_type() -> String {
    "code".into()
}

pub(crate) async fn authorize(
    State(state): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Query(q): Query<AuthorizeQuery>,
) -> Result<Response, AppError> {
    if q.response_type != "code" {
        return Err(AppError::BadRequest("unsupported_response_type".into()));
    }
    if q.code_challenge_method != "S256" {
        return Err(AppError::BadRequest(
            "unsupported code_challenge_method (S256 only)".into(),
        ));
    }
    let repo = crate::repos::OauthClientRepo::new(state.pool.clone());
    let client = repo
        .get_by_client_id(&q.client_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("unknown client_id".into()))?;

    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Connect {name} to IONe</title></head>
<body style="font-family: system-ui; max-width: 420px; margin: 48px auto; padding: 0 16px;">
<h1 style="font-size:18px;">Connect {name} to IONe?</h1>
<p>This client will be able to call IONe's MCP tools on your behalf.</p>
<form method="POST" action="/mcp/oauth/authorize">
  <input type="hidden" name="client_id" value="{client_id}" />
  <input type="hidden" name="redirect_uri" value="{redirect_uri}" />
  <input type="hidden" name="code_challenge" value="{challenge}" />
  <input type="hidden" name="code_challenge_method" value="{method}" />
  <input type="hidden" name="scope" value="{scope}" />
  <input type="hidden" name="state" value="{st}" />
  <button type="submit" name="action" value="allow" style="padding:10px 16px;">Allow</button>
  <button type="submit" name="action" value="deny" style="padding:10px 16px;">Deny</button>
</form></body></html>"#,
        name = html_escape(&client.display_name),
        client_id = html_escape(&q.client_id),
        redirect_uri = html_escape(&q.redirect_uri),
        challenge = html_escape(&q.code_challenge),
        method = html_escape(&q.code_challenge_method),
        scope = html_escape(q.scope.as_deref().unwrap_or("")),
        st = html_escape(q.state.as_deref().unwrap_or("")),
    );
    let _ = ctx;
    Ok(Html(html).into_response())
}

#[derive(Deserialize)]
pub(crate) struct AuthorizeForm {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    #[serde(default = "default_challenge_method")]
    pub code_challenge_method: String,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub action: String,
}

pub(crate) async fn authorize_consent(
    State(state): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Form(form): Form<AuthorizeForm>,
) -> Result<Response, AppError> {
    let redirect = form.redirect_uri.clone();
    let st = form.state.clone().unwrap_or_default();
    if form.action != "allow" {
        let url = if st.is_empty() {
            format!("{redirect}?error=access_denied")
        } else {
            format!("{redirect}?error=access_denied&state={st}")
        };
        return Ok(Redirect::to(&url).into_response());
    }

    let code = generate_opaque_token(32);
    let scope = form.scope.clone().unwrap_or_default();
    let token_repo = crate::repos::OauthTokenRepo::new(state.pool.clone());
    token_repo
        .insert_auth_code(
            &code,
            &form.client_id,
            ctx.user_id,
            &form.redirect_uri,
            &scope,
            &form.code_challenge,
            &form.code_challenge_method,
            AUTH_CODE_TTL_SECS,
        )
        .await
        .map_err(AppError::Internal)?;
    let url = if st.is_empty() {
        format!("{redirect}?code={code}")
    } else {
        format!("{redirect}?code={code}&state={st}")
    };
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize)]
#[serde(tag = "grant_type", rename_all = "snake_case")]
pub(crate) enum TokenBody {
    AuthorizationCode {
        code: String,
        code_verifier: String,
        client_id: String,
        redirect_uri: Option<String>,
    },
    RefreshToken {
        refresh_token: String,
        client_id: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenResp {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub scope: String,
}

pub(crate) async fn token(
    State(state): State<AppState>,
    Form(body): Form<TokenBody>,
) -> Result<Json<TokenResp>, AppError> {
    let token_repo = crate::repos::OauthTokenRepo::new(state.pool.clone());
    let (client_id, user_id, scope) = match body {
        TokenBody::AuthorizationCode {
            code,
            code_verifier,
            client_id,
            redirect_uri,
        } => {
            let row = token_repo
                .consume_auth_code(&code)
                .await
                .map_err(AppError::Internal)?
                .ok_or_else(|| {
                    AppError::BadRequest("invalid_grant: code missing/expired/used".into())
                })?;
            if row.client_id != client_id {
                return Err(AppError::BadRequest(
                    "invalid_grant: client mismatch".into(),
                ));
            }
            if let Some(ru) = redirect_uri {
                if ru != row.redirect_uri {
                    return Err(AppError::BadRequest(
                        "invalid_grant: redirect_uri mismatch".into(),
                    ));
                }
            }
            let verifier_hash =
                general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
            if verifier_hash != row.code_challenge {
                return Err(AppError::BadRequest(
                    "invalid_grant: PKCE verifier mismatch".into(),
                ));
            }
            (row.client_id, row.user_id, row.scope)
        }
        TokenBody::RefreshToken {
            refresh_token,
            client_id,
        } => {
            let hash = sha256_hex(&refresh_token);
            let row = token_repo
                .consume_refresh_token(&hash)
                .await
                .map_err(AppError::Internal)?
                .ok_or_else(|| {
                    AppError::BadRequest("invalid_grant: refresh token missing/expired/used".into())
                })?;
            if row.client_id != client_id {
                return Err(AppError::BadRequest(
                    "invalid_grant: client mismatch".into(),
                ));
            }
            (row.client_id, row.user_id, row.scope)
        }
    };

    let access = generate_opaque_token(32);
    let refresh = generate_opaque_token(32);
    token_repo
        .insert_access_token(
            &sha256_hex(&access),
            &client_id,
            user_id,
            &scope,
            ACCESS_TOKEN_TTL_SECS,
        )
        .await
        .map_err(AppError::Internal)?;
    token_repo
        .insert_refresh_token(
            &sha256_hex(&refresh),
            &client_id,
            user_id,
            &scope,
            REFRESH_TOKEN_TTL_SECS,
        )
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(TokenResp {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL_SECS,
        scope,
    }))
}

#[derive(Deserialize)]
pub(crate) struct RevokeBody {
    pub token: String,
    #[serde(default)]
    pub token_type_hint: Option<String>,
}

pub(crate) async fn revoke(
    State(state): State<AppState>,
    Form(body): Form<RevokeBody>,
) -> StatusCode {
    let _ = body.token_type_hint;
    let hash = sha256_hex(&body.token);
    let pool = state.pool.clone();
    let _ = sqlx::query(
        "UPDATE oauth_access_tokens SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&hash)
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "UPDATE oauth_refresh_tokens SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&hash)
    .execute(&pool)
    .await;
    StatusCode::OK
}

fn generate_opaque_token(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
