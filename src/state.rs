use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::Config,
    connectors::peer_session::PeerSessionRegistry,
    models::Peer,
    services::{
        federation::{PeerManifest, PeerMcpSession, SliceEntry},
        interaction_sink::{InteractionSink, InteractionWriterRx},
        ollama::OllamaClient,
        peer_governor::PeerGovernor,
        pipeline_bus::PipelineBus,
    },
};

/// Identity of a cached peer payload: the peer, plus the credential scope the
/// payload was fetched under.
///
/// Peer manifests and context slices are *responses to an authenticated
/// request*, and a peer is free to answer `tools/list` / `resources/list` /
/// `slice://` differently per credential — that is the entire point of the
/// per-(workspace, peer) delegated token (#12) and static credential (#19).
/// Keying these caches by `peer_id` alone therefore lets one workspace's answer
/// be served to another, so the key carries the scope the fetch presented:
/// `Some(workspace_id)` for a workspace-scoped fetch, `None` for a peer-global
/// one.
///
/// The scope is the *resolution input*, not the resolved bearer. Two workspaces
/// with no scoped credential do both fall through to the same peer-global tier
/// and could safely share an entry, but keying on the bearer would mean
/// resolving (and possibly refreshing) a token before every cache read, and
/// would put credential material in a map key. Keying on the workspace is
/// strictly more conservative: equal keys always imply equal credential
/// resolution, so no entry can cross a scope boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerCacheKey {
    pub peer_id: Uuid,
    pub workspace_scope: Option<Uuid>,
}

impl PeerCacheKey {
    /// The entry produced by a fetch that presented no workspace scope.
    pub fn global(peer_id: Uuid) -> Self {
        Self {
            peer_id,
            workspace_scope: None,
        }
    }

    /// The entry produced by a fetch made in `workspace_id`'s scope.
    pub fn for_workspace(peer_id: Uuid, workspace_id: Uuid) -> Self {
        Self {
            peer_id,
            workspace_scope: Some(workspace_id),
        }
    }

    /// The entry a fetch with this peer handle produces. The handle's
    /// `workspace_scope` is exactly what `peer_tokens` resolves the outbound
    /// bearer from, so deriving the key from it keeps the two in lockstep.
    pub fn for_peer(peer: &Peer) -> Self {
        Self {
            peer_id: peer.id,
            workspace_scope: peer.workspace_scope,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    pub ollama: Arc<OllamaClient>,
    pub pipeline_bus: Arc<PipelineBus>,
    pub interaction_sink: Arc<InteractionSink>,
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub default_user_id: Uuid,
    pub default_workspace_id: Uuid,
    pub peer_manifest_cache: Arc<dashmap::DashMap<PeerCacheKey, PeerManifest>>,
    pub peer_slice_cache: Arc<dashmap::DashMap<PeerCacheKey, SliceEntry>>,
    /// Outbound MCP session ids, one per peer handle. Keyed by `PeerCacheKey`
    /// for the same reason the two caches above are: the id is bound to the
    /// credential the `initialize` presented, so it may not cross a scope.
    pub peer_mcp_sessions: Arc<dashmap::DashMap<PeerCacheKey, PeerMcpSession>>,
    pub peer_sessions: Arc<PeerSessionRegistry>,
    pub peer_governor: Arc<dashmap::DashMap<Uuid, Arc<PeerGovernor>>>,
    pub mcp_sessions: Arc<dashmap::DashMap<String, serde_json::Value>>,
    /// Per-peer mutex preventing concurrent token refresh (token-overwrite race fix).
    pub peer_refresh_locks: Arc<dashmap::DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-org single-flight for audit exports: occupied entry = export in progress.
    pub export_locks: Arc<dashmap::DashMap<Uuid, ()>>,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        default_user_id: Uuid,
        default_workspace_id: Uuid,
    ) -> Self {
        Self::new_parts(config, pool, default_user_id, default_workspace_id).0
    }

    pub fn new_parts(
        config: Config,
        pool: PgPool,
        default_user_id: Uuid,
        default_workspace_id: Uuid,
    ) -> (Self, InteractionWriterRx) {
        let http = crate::util::url_guard::guarded_client(15_000);
        let ollama = Arc::new(OllamaClient::new(config.ollama_base_url.clone()));
        let pipeline_bus = Arc::new(PipelineBus::new());
        let (interaction_sink, interaction_rx) = InteractionSink::new();
        (
            Self {
                http,
                ollama,
                pipeline_bus,
                interaction_sink,
                config: Arc::new(config),
                pool,
                default_user_id,
                default_workspace_id,
                peer_manifest_cache: Arc::new(dashmap::DashMap::new()),
                peer_slice_cache: Arc::new(dashmap::DashMap::new()),
                peer_mcp_sessions: Arc::new(dashmap::DashMap::new()),
                peer_sessions: Arc::new(PeerSessionRegistry::default()),
                peer_governor: Arc::new(dashmap::DashMap::new()),
                mcp_sessions: Arc::new(dashmap::DashMap::new()),
                peer_refresh_locks: Arc::new(dashmap::DashMap::new()),
                export_locks: Arc::new(dashmap::DashMap::new()),
            },
            interaction_rx,
        )
    }
}
