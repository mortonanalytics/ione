pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod repos;
pub mod routes;
pub mod services;
pub mod state;

use axum::Router;
use sqlx::PgPool;
use uuid::Uuid;

/// Construct the application router with a live database pool.
/// Runs the idempotent bootstrap (default org + user) on every call.
pub async fn app(pool: PgPool) -> Router {
    let (_, default_user_id) = repos::bootstrap::ensure_default_org_and_user(&pool)
        .await
        .expect("bootstrap failed");

    let config = config::Config::from_env();
    let state = state::AppState::new(config, pool, default_user_id);
    routes::router(state)
}

/// Phase-1 compatibility shim: boot without an active database connection.
///
/// Uses a lazy pool (never actually connects unless a DB handler is exercised)
/// and a nil UUID as the default user id. Safe for Phase 1 tests that only hit
/// `/api/v1/health`, `/api/v1/chat`, and static assets.
pub async fn app_no_db() -> Router {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://ione:ione@localhost:5433/ione".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&db_url)
        .expect("connect_lazy failed");
    let config = config::Config::from_env();
    let state = state::AppState::new(config, pool, Uuid::nil());
    routes::router(state)
}
