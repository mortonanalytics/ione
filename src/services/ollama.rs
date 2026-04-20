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
        let url = format!("{}/api/generate", self.base_url);
        let start = std::time::Instant::now();

        let resp = self
            .http
            .post(&url)
            .json(&json!({ "model": model, "prompt": prompt, "stream": false }))
            .send()
            .await
            .map_err(|e| AppError::OllamaUpstream(format!("request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::OllamaUpstream(format!(
                "upstream returned {status}: {body}"
            )));
        }

        let parsed: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| AppError::OllamaUpstream(format!("failed to parse response: {e}")))?;

        info!(model = %model, elapsed_ms = start.elapsed().as_millis(), "ollama generate complete");

        Ok(parsed.response)
    }
}
