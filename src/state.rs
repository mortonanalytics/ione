use std::sync::Arc;

use crate::{config::Config, services::ollama::OllamaClient};

#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    pub ollama: Arc<OllamaClient>,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let http = reqwest::Client::new();
        let ollama = Arc::new(OllamaClient::new(config.ollama_base_url.clone()));
        Self {
            http,
            ollama,
            config: Arc::new(config),
        }
    }
}
