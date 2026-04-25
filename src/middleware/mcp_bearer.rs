use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Clone, Debug)]
pub struct OauthContext {
    pub user_id: Uuid,
    pub client_id: String,
    pub scope: String,
}

pub async fn mcp_bearer(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string());

    let Some(token) = token else {
        return unauthorized(&state);
    };

    if let Ok(expected) = std::env::var("IONE_OAUTH_STATIC_BEARER") {
        if !expected.is_empty() && token.as_bytes().ct_eq(expected.as_bytes()).into() {
            req.extensions_mut().insert(OauthContext {
                user_id: state.default_user_id,
                client_id: "static".to_string(),
                scope: "mcp".to_string(),
            });
            return next.run(req).await;
        }
    }

    let hash = sha256_hex(&token);
    let repo = crate::repos::OauthTokenRepo::new(state.pool.clone());
    match repo.find_access_token(&hash).await {
        Ok(Some(row)) => {
            let client_id = row.client_id.clone();
            let client_repo = crate::repos::OauthClientRepo::new(state.pool.clone());
            tokio::spawn(async move {
                let _ = client_repo.touch_last_seen(&client_id).await;
            });
            req.extensions_mut().insert(OauthContext {
                user_id: row.user_id,
                client_id: row.client_id,
                scope: row.scope,
            });
            next.run(req).await
        }
        _ => unauthorized(&state),
    }
}

fn unauthorized(state: &AppState) -> Response {
    let resource_metadata = format!(
        "{}/.well-known/oauth-protected-resource",
        state.config.oauth_issuer
    );
    let challenge = format!(r#"Bearer realm="ione", resource_metadata="{resource_metadata}""#);
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "message": "MCP access requires a valid Bearer token."
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, value);
    }
    resp
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
