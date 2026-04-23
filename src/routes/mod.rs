use axum::{
    middleware::from_fn,
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use tower_http::{cors::{Any, CorsLayer}, services::ServeDir, trace::TraceLayer};

use crate::{
    auth::auth_middleware,
    mcp_server,
    middleware::demo_guard::demo_write_guard,
    state::AppState,
};

pub mod approvals;
pub mod artifacts;
pub mod audit_events;
pub mod auth_routes;
pub mod chat;
pub mod connectors;
pub mod conversations;
pub mod feed;
pub mod health;
pub mod me;
pub mod peers;
pub mod signals;
pub mod survivors;
pub mod workspaces;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();

    // Routes that are always public (no auth middleware).
    let public = Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/health/ollama", get(health::health_ollama))
        .route("/auth/login", get(auth_routes::login))
        .route("/auth/callback", get(auth_routes::callback))
        .route("/auth/logout", post(auth_routes::logout))
        .with_state(state.clone());

    // Routes that run through the auth middleware.
    let protected = Router::new()
        .route("/api/v1/chat", post(chat::chat))
        .route(
            "/api/v1/conversations",
            get(conversations::list_conversations).post(conversations::create_conversation),
        )
        .route(
            "/api/v1/conversations/:id",
            get(conversations::get_conversation),
        )
        .route(
            "/api/v1/conversations/:id/messages",
            post(conversations::post_message),
        )
        .route(
            "/api/v1/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
        .route("/api/v1/workspaces/:id", get(workspaces::get_workspace))
        .route(
            "/api/v1/workspaces/:id/close",
            post(workspaces::close_workspace),
        )
        .route(
            "/api/v1/workspaces/:id/connectors",
            get(connectors::list_connectors).post(connectors::create_connector),
        )
        .route(
            "/api/v1/connectors/:id/streams",
            get(connectors::list_streams),
        )
        .route("/api/v1/streams/:id/poll", post(connectors::poll_stream))
        .route("/api/v1/workspaces/:id/signals", get(signals::list_signals))
        .route(
            "/api/v1/workspaces/:id/survivors",
            get(survivors::list_survivors),
        )
        .route("/api/v1/workspaces/:id/feed", get(feed::get_feed))
        .route("/api/v1/workspaces/:id/roles", get(workspaces::list_roles))
        .route(
            "/api/v1/workspaces/:id/artifacts",
            get(artifacts::list_artifacts),
        )
        .route(
            "/api/v1/workspaces/:id/approvals",
            get(approvals::list_approvals),
        )
        .route(
            "/api/v1/workspaces/:id/audit_events",
            get(audit_events::list_audit_events),
        )
        .route("/api/v1/approvals/:id", post(approvals::decide_approval))
        .route(
            "/api/v1/peers",
            get(peers::list_peers).post(peers::create_peer),
        )
        .route(
            "/api/v1/workspaces/:id/peers/:peerId/subscribe",
            post(peers::subscribe_peer),
        )
        .route("/api/v1/me", get(me::me))
        .route_layer(from_fn(demo_write_guard))
        .route_layer(from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // MCP server routes — no auth_middleware; MCP handles its own auth per tool call.
    let mcp = mcp_server::router().with_state(state.clone());

    Router::new()
        .merge(public)
        .merge(protected)
        .merge(mcp)
        .nest_service("/", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}
