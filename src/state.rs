use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::Config,
    services::{ollama::OllamaClient, pipeline_bus::PipelineBus},
};

#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    pub ollama: Arc<OllamaClient>,
    pub pipeline_bus: Arc<PipelineBus>,
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub default_user_id: Uuid,
    pub default_workspace_id: Uuid,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        default_user_id: Uuid,
        default_workspace_id: Uuid,
    ) -> Self {
        let http = reqwest::Client::new();
        let ollama = Arc::new(OllamaClient::new(config.ollama_base_url.clone()));
        let pipeline_bus = Arc::new(PipelineBus::new());
        Self {
            http,
            ollama,
            pipeline_bus,
            config: Arc::new(config),
            pool,
            default_user_id,
            default_workspace_id,
        }
    }
}
