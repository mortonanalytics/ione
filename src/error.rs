use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unprocessable entity: {0}")]
    UnprocessableEntity(String),

    #[error("unprocessable entity: {0}")]
    UnprocessableEntityJson(serde_json::Value),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("webhook rejected")]
    WebhookRejected,

    #[error("webhook unauthorized")]
    WebhookUnauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("mfa required")]
    MfaRequired,

    #[error("mfa enrollment required")]
    MfaEnrollmentRequired,

    #[error("ollama upstream error: {0}")]
    OllamaUpstream(String),

    #[error("ollama unreachable at {base_url}: {error}")]
    OllamaUnreachable { base_url: String, error: String },

    #[error("ollama model missing: {model}")]
    OllamaModelMissing { model: String, pull_command: String },

    #[error("connector error: {0}")]
    ConnectorError(String),

    #[error("payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("too many requests: {0}")]
    TooManyRequests(String),

    #[error("workspace binding conflict: foreign_tenant_id changed from {old} to {new}")]
    WorkspaceBindingConflict { old: String, new: String },

    #[error("whoami unreachable for peer {peer_id}: {message}")]
    WhoamiUnreachable {
        peer_id: uuid::Uuid,
        message: String,
    },

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "bad_request",
                    "message": msg,
                })),
            )
                .into_response(),
            AppError::UnprocessableEntity(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "unprocessable_entity",
                    "message": msg,
                })),
            )
                .into_response(),
            AppError::UnprocessableEntityJson(value) => {
                (StatusCode::UNPROCESSABLE_ENTITY, Json(value)).into_response()
            }
            AppError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "not_found",
                    "message": msg,
                    "hint": "Check the URL or create the resource first."
                })),
            )
                .into_response(),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "unauthorized",
                    "message": "Sign in to access this resource.",
                    "hint": "Try signing in again."
                })),
            )
                .into_response(),
            AppError::WebhookRejected => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "webhook_rejected"
                })),
            )
                .into_response(),
            AppError::WebhookUnauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "webhook_unauthorized"
                })),
            )
                .into_response(),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "forbidden",
                    "message": "You don't have permission to perform this action."
                })),
            )
                .into_response(),
            AppError::MfaRequired => (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "mfa_required",
                    "message": "Complete MFA challenge to continue."
                })),
            )
                .into_response(),
            AppError::MfaEnrollmentRequired => (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "mfa_enrollment_required",
                    "message": "Enroll MFA before using this endpoint."
                })),
            )
                .into_response(),
            AppError::OllamaUpstream(msg) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "ollama_upstream",
                    "message": msg,
                    "hint": "Check Ollama server logs or try again in a moment."
                })),
            )
                .into_response(),
            AppError::OllamaUnreachable { base_url, error: _ } => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "ollama_unreachable",
                    "message": format!("Ollama is not reachable at {}. Start Ollama and try again.", base_url),
                    "baseUrl": base_url,
                    "hint": "Run 'ollama serve' or set OLLAMA_BASE_URL to the correct host."
                })),
            )
                .into_response(),
            AppError::OllamaModelMissing {
                model,
                pull_command,
            } => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "ollama_model_missing",
                    "message": format!("Ollama doesn't have the '{}' model pulled.", model),
                    "model": model,
                    "pullCommand": pull_command,
                    "hint": "Run the pullCommand in a terminal."
                })),
            )
                .into_response(),
            AppError::ConnectorError(msg) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "connector_error",
                    "message": msg,
                })),
            )
                .into_response(),
            AppError::PayloadTooLarge(msg) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": "payload_too_large",
                    "message": msg,
                })),
            )
                .into_response(),
            AppError::TooManyRequests(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "error": "too_many_requests",
                    "message": msg,
                })),
            )
                .into_response(),
            AppError::WorkspaceBindingConflict { old, new } => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "workspace_binding_conflict",
                    "message": format!("foreign_tenant_id changed from {} to {}", old, new),
                    "old": old,
                    "new": new,
                })),
            )
                .into_response(),
            AppError::WhoamiUnreachable { peer_id, message } => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "whoami_unreachable",
                    "message": format!("whoami unreachable for peer {}: {}", peer_id, message),
                    "peerId": peer_id,
                })),
            )
                .into_response(),
            AppError::Internal(e) => {
                tracing::error!(error = %e, "internal application error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "internal",
                        "message": "Internal server error"
                    })),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn every_variant_has_nonempty_error_and_message() {
        let cases: Vec<AppError> = vec![
            AppError::BadRequest("x".into()),
            AppError::UnprocessableEntity("x".into()),
            AppError::NotFound("y".into()),
            AppError::Unauthorized,
            AppError::WebhookRejected,
            AppError::WebhookUnauthorized,
            AppError::Forbidden,
            AppError::MfaRequired,
            AppError::MfaEnrollmentRequired,
            AppError::OllamaUpstream("upstream failed".into()),
            AppError::OllamaUnreachable {
                base_url: "http://localhost:11434".into(),
                error: "connection refused".into(),
            },
            AppError::OllamaModelMissing {
                model: "llama3.2".into(),
                pull_command: "ollama pull llama3.2".into(),
            },
            AppError::ConnectorError("connector failed".into()),
            AppError::PayloadTooLarge("too much".into()),
            AppError::TooManyRequests("one export at a time".into()),
            AppError::WorkspaceBindingConflict {
                old: "old".into(),
                new: "new".into(),
            },
            AppError::WhoamiUnreachable {
                peer_id: uuid::Uuid::nil(),
                message: "offline".into(),
            },
            AppError::Internal(anyhow::anyhow!("test")),
        ];

        for err in cases {
            let webhook_error = matches!(
                err,
                AppError::WebhookRejected | AppError::WebhookUnauthorized
            );
            let resp = err.into_response();
            let (parts, body) = resp.into_parts();
            let bytes = axum::body::to_bytes(body, 4096).await.expect("body");
            let v: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            assert!(
                v["error"].as_str().filter(|s| !s.is_empty()).is_some(),
                "{parts:?} / {v}"
            );
            assert!(
                webhook_error || v["message"].as_str().filter(|s| !s.is_empty()).is_some(),
                "{parts:?} / {v}"
            );
        }
    }
}
