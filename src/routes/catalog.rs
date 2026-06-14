use axum::{
    extract::{Extension, Path, Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::AuthContext, error::AppError, models::CatalogEntryKind, repos::CatalogSearchRow,
    services::catalog::CatalogService, state::AppState,
};

#[derive(Deserialize)]
pub struct CatalogSearchQuery {
    pub q: String,
    pub kind: Option<String>,
    pub limit: Option<i64>,
}

fn parse_kind(kind: Option<&str>) -> Result<Option<CatalogEntryKind>, AppError> {
    match kind {
        None => Ok(None),
        Some("tool") => Ok(Some(CatalogEntryKind::Tool)),
        Some("resource") => Ok(Some(CatalogEntryKind::Resource)),
        Some(other) => Err(AppError::BadRequest(format!("invalid kind '{other}'"))),
    }
}

fn item_json(row: &CatalogSearchRow) -> Value {
    json!({
        "peer_id": row.peer_id,
        "peer_name": row.peer_name,
        "namespaced_name": row.namespaced_name,
        "kind": row.kind,
        "description": row.description,
        "sample_queries": row.sample_queries,
        "score": row.score,
    })
}

/// `GET /api/v1/workspaces/:id/catalog-search?q=…&kind=tool|resource&limit=int`.
/// Results are pre-filtered to the caller's invokable set (FCS-C1).
pub async fn catalog_search(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
    Query(params): Query<CatalogSearchQuery>,
) -> Result<Json<Value>, AppError> {
    let kind = parse_kind(params.kind.as_deref())?;
    let rows =
        CatalogService::search(&state, workspace_id, &auth, &params.q, kind, params.limit).await?;
    let items: Vec<Value> = rows.iter().map(item_json).collect();
    Ok(Json(json!({ "items": items })))
}
