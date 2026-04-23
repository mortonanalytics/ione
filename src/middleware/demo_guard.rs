//! Write-guard for the demo workspace.
//!
//! Rejects any non-GET/HEAD request whose URL resolves to a demo-workspace
//! path with 403 `demo_read_only`. Applied as a route layer after auth,
//! so it cannot be bypassed by callers that already have a session.

use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use uuid::Uuid;

use crate::demo::DEMO_WORKSPACE_ID;

/// Axum middleware that short-circuits mutating requests to demo-workspace
/// resources with a 403 `demo_read_only` envelope.
pub async fn demo_write_guard(req: Request<Body>, next: Next) -> Response {
    let method = req.method();
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if let Some(ws_id) = extract_workspace_id_from_path(path) {
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

/// Parse `/api/v1/workspaces/<uuid>/...` and return the UUID, else None.
fn extract_workspace_id_from_path(path: &str) -> Option<Uuid> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.len() >= 4 && parts[0] == "api" && parts[1] == "v1" && parts[2] == "workspaces" {
        Uuid::parse_str(parts[3]).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_uuid_from_known_shape() {
        let id = extract_workspace_id_from_path(
            "/api/v1/workspaces/00000000-0000-0000-0000-000000000d30/connectors",
        )
        .unwrap();
        assert_eq!(id, DEMO_WORKSPACE_ID);
    }

    #[test]
    fn returns_none_for_unrelated_paths() {
        assert!(extract_workspace_id_from_path("/api/v1/conversations/123").is_none());
        assert!(extract_workspace_id_from_path("/health").is_none());
    }

    #[test]
    fn returns_none_for_non_uuid_segment() {
        assert!(extract_workspace_id_from_path("/api/v1/workspaces/not-a-uuid/x").is_none());
    }
}
