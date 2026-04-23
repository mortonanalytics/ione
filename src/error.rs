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
            AppError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
            }
            AppError::OllamaUpstream(msg) => {
                (StatusCode::BAD_GATEWAY, Json(json!({ "error": msg }))).into_response()
            }
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
            AppError::ConnectorError(msg) => {
                (StatusCode::BAD_GATEWAY, Json(json!({ "error": msg }))).into_response()
            }
            AppError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        }
    }
}
