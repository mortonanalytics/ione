//! The server exits 0 on SIGTERM instead of dying by signal (#128).
//!
//! Run: cargo test --test graceful_shutdown_integration -- --ignored --test-threads=1

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

#[tokio::test]
#[ignore]
async fn sigterm_exits_zero_within_five_seconds() {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("local addr")
        .port();
    let bind = format!("127.0.0.1:{port}");

    let mut child = Command::new(env!("CARGO_BIN_EXE_ione"))
        .env("DATABASE_URL", db_url)
        .env("IONE_BIND", &bind)
        .env("IONE_TOKEN_KEY", TEST_KEY)
        .env("IONE_WEBHOOK_SECRET_KEY", TEST_KEY)
        .env("IONE_AUTH_MODE", "local")
        .env("OLLAMA_BASE_URL", "http://127.0.0.1:9")
        .env_remove("IONE_SEED_DEMO")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ione");

    let health = format!("http://{bind}/api/v1/health");
    let started = Instant::now();
    loop {
        if let Ok(resp) = reqwest::get(&health).await {
            if resp.status().is_success() {
                break;
            }
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("ione exited before becoming healthy: {status}");
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("ione did not become healthy within 60 s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(killed.success(), "kill -TERM failed");

    let signalled = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if signalled.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("ione was still running 5 s after SIGTERM");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must end in a clean exit, not death by signal: {status}"
    );
}
