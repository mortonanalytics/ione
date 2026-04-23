pub mod auth;
pub mod config;
pub mod connectors;
pub mod db;
pub mod demo;
pub mod error;
pub mod mcp_server;
pub mod models;
pub mod repos;
pub mod routes;
pub mod services;
pub mod state;

use axum::Router;
use sqlx::PgPool;
use uuid::Uuid;

/// Construct the application router with a live database pool.
/// Runs the idempotent bootstrap (default org + user + workspace + membership)
/// on every call.
pub async fn app(pool: PgPool) -> Router {
    let (router, _state) = app_with_state(pool).await;
    router
}

/// Like `app`, but also returns the `AppState` so callers can spawn the scheduler.
pub async fn app_with_state(pool: PgPool) -> (Router, state::AppState) {
    let (org_id, default_user_id) = repos::bootstrap::ensure_default_org_and_user(&pool)
        .await
        .expect("bootstrap failed");

    let default_workspace_id =
        repos::bootstrap::ensure_default_workspace_and_membership(&pool, org_id, default_user_id)
            .await
            .expect("workspace bootstrap failed");

    let config = config::Config::from_env();
    let app_state = state::AppState::new(config, pool, default_user_id, default_workspace_id);
    let router = routes::router(app_state.clone());
    (router, app_state)
}

/// Phase-1 compatibility shim: boot without an active database connection.
///
/// Uses a lazy pool (never actually connects unless a DB handler is exercised)
/// and nil UUIDs as the default user/workspace ids. Safe for Phase 1 tests that
/// only hit `/api/v1/health`, `/api/v1/chat`, and static assets.
pub async fn app_no_db() -> Router {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://ione:ione@localhost:5433/ione".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&db_url)
        .expect("connect_lazy failed");
    let config = config::Config::from_env();
    let state = state::AppState::new(config, pool, Uuid::nil(), Uuid::nil());
    routes::router(state)
}
