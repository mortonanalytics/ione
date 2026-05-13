use axum::{extract::State, response::Json};
use serde::Serialize;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicIssuerResp {
    pub id: Uuid,
    pub display_name: String,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<PublicIssuerResp>>, AppError> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "SELECT id, issuer_url, display_name FROM trust_issuers ORDER BY issuer_url",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let items = rows
        .into_iter()
        .map(|(id, issuer_url, display_name)| PublicIssuerResp {
            id,
            display_name: display_name.unwrap_or_else(|| issuer_host(&issuer_url)),
        })
        .collect();
    Ok(Json(items))
}

fn issuer_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}
