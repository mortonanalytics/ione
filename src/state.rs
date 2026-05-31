use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::Config,
    connectors::peer_session::PeerSessionRegistry,
    services::{
        federation::{PeerManifest, SliceEntry},
        ollama::OllamaClient,
        peer_governor::PeerGovernor,
        pipeline_bus::PipelineBus,
    },
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
    pub peer_manifest_cache: Arc<dashmap::DashMap<Uuid, PeerManifest>>,
    pub peer_slice_cache: Arc<dashmap::DashMap<Uuid, SliceEntry>>,
    pub peer_sessions: Arc<PeerSessionRegistry>,
    pub peer_governor: Arc<dashmap::DashMap<Uuid, Arc<PeerGovernor>>>,
    pub mcp_sessions: Arc<dashmap::DashMap<String, serde_json::Value>>,
    /// Per-peer mutex preventing concurrent token refresh (token-overwrite race fix).
    pub peer_refresh_locks: Arc<dashmap::DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        default_user_id: Uuid,
        default_workspace_id: Uuid,
    ) -> Self {
        let http = crate::util::url_guard::guarded_client(15_000);
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
            peer_manifest_cache: Arc::new(dashmap::DashMap::new()),
            peer_slice_cache: Arc::new(dashmap::DashMap::new()),
            peer_sessions: Arc::new(PeerSessionRegistry::default()),
            peer_governor: Arc::new(dashmap::DashMap::new()),
            mcp_sessions: Arc::new(dashmap::DashMap::new()),
            peer_refresh_locks: Arc::new(dashmap::DashMap::new()),
        }
    }
}
