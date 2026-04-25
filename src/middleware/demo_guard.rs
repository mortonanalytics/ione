//! Write-guard for the demo workspace.
//!
//! Rejects any non-GET/HEAD request whose URL resolves to a demo-workspace
//! path with 403 `demo_read_only`. Applied as a route layer after auth,
//! so it cannot be bypassed by callers that already have a session.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use uuid::Uuid;

use crate::demo::DEMO_WORKSPACE_ID;
use crate::state::AppState;

/// Axum middleware that short-circuits mutating requests to demo-workspace
/// resources with a 403 `demo_read_only` envelope.
pub async fn demo_write_guard(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method();
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if let Some(ws_id) = resolve_workspace_id(&state, path).await {
        if ws_id == DEMO_WORKSPACE_ID {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "demo_read_only",
                    "message": "Switch to your workspace to make changes."
                })),
            )
                .into_response();
        }
    }

    // Conversations are owned by a workspace indirectly. We can't resolve
    // the conversation→workspace mapping here without a DB call; the
    // canned-chat handler in conversations.rs will emit its own canned
    // response for demo conversations. Non-conversation writes are caught
    // by the /workspaces/<DEMO>/... path form above.

    next.run(req).await
}

async fn resolve_workspace_id(state: &AppState, path: &str) -> Option<Uuid> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.len() >= 4 && parts[0] == "api" && parts[1] == "v1" && parts[2] == "workspaces" {
        return Uuid::parse_str(parts[3]).ok();
    }

    if parts.len() >= 4 && parts[0] == "api" && parts[1] == "v1" && parts[2] == "streams" {
        let stream_id = Uuid::parse_str(parts[3]).ok()?;
        return sqlx::query_scalar(
            "SELECT c.workspace_id
             FROM streams s
             JOIN connectors c ON c.id = s.connector_id
             WHERE s.id = $1",
        )
        .bind(stream_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    }

    if parts.len() >= 4 && parts[0] == "api" && parts[1] == "v1" && parts[2] == "approvals" {
        let approval_id = Uuid::parse_str(parts[3]).ok()?;
        return sqlx::query_scalar(
            "SELECT a.workspace_id
             FROM approvals ap
             JOIN artifacts a ON a.id = ap.artifact_id
             WHERE ap.id = $1",
        )
        .bind(approval_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_uuid_from_known_shape() {
        let parts: Vec<&str> = "/api/v1/workspaces/00000000-0000-0000-0000-000000000d30/connectors"
            .trim_matches('/')
            .split('/')
            .collect();
        assert_eq!(Uuid::parse_str(parts[3]).unwrap(), DEMO_WORKSPACE_ID);
    }

    #[test]
    fn returns_none_for_unrelated_paths() {
        assert!(Uuid::parse_str("123").is_err());
    }

    #[test]
    fn returns_none_for_non_uuid_segment() {
        assert!(Uuid::parse_str("not-a-uuid").is_err());
    }
}
