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

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("ollama upstream error: {0}")]
    OllamaUpstream(String),

    #[error("ollama unreachable at {base_url}: {error}")]
    OllamaUnreachable { base_url: String, error: String },

    #[error("ollama model missing: {model}")]
    OllamaModelMissing { model: String, pull_command: String },

    #[error("connector error: {0}")]
    ConnectorError(String),

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
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "forbidden",
                    "message": "You don't have permission to perform this action."
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
            AppError::NotFound("y".into()),
            AppError::Unauthorized,
            AppError::Forbidden,
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
            AppError::Internal(anyhow::anyhow!("test")),
        ];

        for err in cases {
            let resp = err.into_response();
            let (parts, body) = resp.into_parts();
            let bytes = axum::body::to_bytes(body, 4096).await.expect("body");
            let v: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            assert!(
                v["error"].as_str().filter(|s| !s.is_empty()).is_some(),
                "{parts:?} / {v}"
            );
            assert!(
                v["message"].as_str().filter(|s| !s.is_empty()).is_some(),
                "{parts:?} / {v}"
            );
        }
    }
}
