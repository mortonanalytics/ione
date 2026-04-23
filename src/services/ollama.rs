use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tracing::{info, instrument};

use crate::error::AppError;

pub struct OllamaClient {
    pub base_url: String,
    pub http: reqwest::Client,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelTag>,
}

#[derive(Deserialize)]
struct ModelTag {
    name: String,
}

#[derive(Debug)]
pub enum OllamaError {
    Unreachable(String),
    ModelMissing(String),
    Other(String),
}

impl OllamaError {
    pub fn into_app_error(self, base_url: &str) -> AppError {
        match self {
            Self::Unreachable(error) => AppError::OllamaUnreachable {
                base_url: base_url.to_string(),
                error,
            },
            Self::ModelMissing(found_model) => AppError::OllamaModelMissing {
                model: found_model.clone(),
                pull_command: format!("ollama pull {found_model}"),
            },
            Self::Other(message) => AppError::OllamaUpstream(message),
        }
    }
}

impl OllamaClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");
        Self { base_url, http }
    }

    #[instrument(skip(self), fields(model = %model))]
    pub async fn generate(&self, model: &str, prompt: &str) -> Result<String, AppError> {
        self.generate_rich(model, prompt)
            .await
            .map_err(|e| e.into_app_error(&self.base_url))
    }

    #[instrument(skip(self), fields(model = %model))]
    pub async fn generate_rich(&self, model: &str, prompt: &str) -> Result<String, OllamaError> {
        let url = format!("{}/api/generate", self.base_url);
        let start = std::time::Instant::now();

        let resp = self
            .http
            .post(&url)
            .json(&json!({ "model": model, "prompt": prompt, "stream": false }))
            .send()
            .await
            .map_err(|e| OllamaError::Unreachable(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let body_lower = body.to_ascii_lowercase();
            if status == reqwest::StatusCode::NOT_FOUND
                || body_lower.contains("not found")
                || body_lower.contains("pull model")
            {
                return Err(OllamaError::ModelMissing(model.to_string()));
            }
            return Err(OllamaError::Other(format!(
                "upstream returned {status}: {body}"
            )));
        }

        let parsed: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| OllamaError::Other(format!("failed to parse response: {e}")))?;

        info!(model = %model, elapsed_ms = start.elapsed().as_millis(), "ollama generate complete");

        Ok(parsed.response)
    }

    pub async fn list_models(&self) -> Result<Vec<String>, AppError> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| AppError::OllamaUpstream(format!("request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(AppError::OllamaUpstream(format!(
                "upstream {}",
                resp.status()
            )));
        }

        let parsed: TagsResponse = resp
            .json()
            .await
            .map_err(|e| AppError::OllamaUpstream(format!("parse: {e}")))?;

        Ok(parsed.models.into_iter().map(|model| model.name).collect())
    }
}
