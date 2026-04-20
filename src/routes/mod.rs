use axum::{
    routing::{get, post},
    Router,
};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::state::AppState;

pub mod chat;
pub mod connectors;
pub mod conversations;
pub mod feed;
pub mod health;
pub mod signals;
pub mod survivors;
pub mod workspaces;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();

    let api = Router::new()
        .route("/api/v1/health", get(health::health))
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
        .with_state(state);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .merge(api)
        .nest_service("/", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}
