use axum::{extract::State, response::Json};
use serde_json::{json, Value};

use crate::state::AppState;

pub(crate) async fn mcp_client_metadata(State(state): State<AppState>) -> Json<Value> {
    let issuer = state.config.oauth_issuer.clone();
    Json(json!({
        "client_name": "IONe",
        "client_uri": issuer,
        "redirect_uris": [format!("{issuer}/api/v1/peers/callback")],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": "mcp",
        "token_endpoint_auth_method": "none"
    }))
}
