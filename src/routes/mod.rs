use axum::{routing::get, Router};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::state::AppState;

pub mod chat;
pub mod health;

pub fn router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();

    let api = Router::new()
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/chat", axum::routing::post(chat::chat))
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
