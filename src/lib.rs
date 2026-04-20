pub mod config;
pub mod error;
pub mod routes;
pub mod services;
pub mod state;

use axum::Router;

/// Construct the application router. Config is loaded from environment variables.
pub async fn app() -> Router {
    let config = config::Config::from_env();
    let state = state::AppState::new(config);
    routes::router(state)
}
