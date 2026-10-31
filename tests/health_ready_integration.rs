//! `GET /api/v1/health/ready` reports whether Postgres can serve requests,
//! separately from the `/api/v1/health` liveness probe (#101).
//!
//! Run: cargo test --test health_ready_integration -- --ignored --test-threads=1

use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{addr}"), pool)
}

#[tokio::test]
#[ignore]
async fn ready_is_200_with_a_live_pool_and_503_once_the_pool_is_closed() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let live = client
        .get(format!("{base}/api/v1/health/ready"))
        .send()
        .await
        .expect("ready request");
    assert_eq!(live.status(), StatusCode::OK);
    let body: Value = live.json().await.expect("json");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    pool.close().await;

    let down = client
        .get(format!("{base}/api/v1/health/ready"))
        .send()
        .await
        .expect("ready request");
    assert_eq!(down.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = down.json().await.expect("json");
    assert_eq!(body["status"], "unavailable");
    assert_eq!(body["checks"]["database"], "error");

    // Liveness is unaffected by the database.
    let live = client
        .get(format!("{base}/api/v1/health"))
        .send()
        .await
        .expect("health request");
    assert_eq!(live.status(), StatusCode::OK);
}
