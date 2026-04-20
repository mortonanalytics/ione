use axum::{extract::State, response::Json};
use serde::{Deserialize, Serialize};

use crate::{error::AppError, state::AppState};

#[derive(Deserialize)]
pub struct ChatRequest {
    pub prompt: String,
    pub model: Option<String>,
}

#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub model: String,
}

pub(crate) async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let trimmed = req.prompt.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("prompt must not be empty".to_string()));
    }

    let model = req
        .model
        .as_deref()
        .unwrap_or(&state.config.ollama_model)
        .to_string();

    let reply = state.ollama.generate(&model, trimmed).await?;

    Ok(Json(ChatResponse { reply, model }))
}
