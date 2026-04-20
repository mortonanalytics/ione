use axum::{
    extract::{Path, State},
    response::Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{error::AppError, repos::AuditEventRepo, state::AppState};

/// GET /api/v1/workspaces/:id/audit_events
///
/// Returns the 200 most recent audit events for the workspace, descending.
pub async fn list_audit_events(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let repo = AuditEventRepo::new(state.pool.clone());
    let items = repo
        .list_for_workspace(workspace_id, 200)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(json!({ "items": items })))
}
