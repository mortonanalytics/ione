use axum::{
    extract::{Extension, Path, State},
    response::Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{auth::AuthContext, error::AppError, state::AppState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectedClient {
    pub id: Uuid,
    pub client_id: String,
    pub display_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResp {
    pub items: Vec<ConnectedClient>,
}

pub(crate) async fn list_clients(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<ListResp>, AppError> {
    let repo = crate::repos::OauthClientRepo::new(state.pool.clone());
    let rows = repo
        .list_for_user(ctx.user_id)
        .await
        .map_err(AppError::Internal)?;
    let items = rows
        .into_iter()
        .map(|c| ConnectedClient {
            id: c.id,
            client_id: c.client_id,
            display_name: c.display_name,
            created_at: c.created_at,
            last_seen_at: c.last_seen_at,
        })
        .collect();
    Ok(Json(ListResp { items }))
}

pub(crate) async fn revoke_client(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(client_row_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    // The path is the row UUID; resolve to client_id first.
    let client: crate::models::OauthClient = sqlx::query_as(
        "SELECT id, client_id, client_metadata, registered_by_user_id, display_name, created_at, last_seen_at
           FROM oauth_clients WHERE id = $1",
    )
    .bind(client_row_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::BadRequest(format!("mcp client {client_row_id} not found")))?;

    let token_repo = crate::repos::OauthTokenRepo::new(state.pool.clone());
    token_repo
        .revoke_client_tokens(&client.client_id, ctx.user_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(json!({"ok": true})))
}
