/// Phase 1 contract tests — boot the axum server on a random port and exercise
/// both endpoints end-to-end.
///
/// These tests are written against the contract defined in
/// md/design/ione-v1-contract.md and will FAIL at compile time until
/// `ione::app()` is implemented in src/main.rs (or src/lib.rs).
///
/// Canonical field names (from contract):
///   Health response : { status: "ok", version: <string> }
///   ChatRequest     : { model?: string, prompt: string }
///   ChatResponse    : { reply: string, model: string }
///
/// Running the Ollama-dependent test:
///   OLLAMA_BASE_URL=http://localhost:11434 cargo test --test phase01_chat -- --ignored
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::net::TcpListener;

/// Boots the app on a random OS-assigned port and returns the base URL.
/// Expects the crate to expose `ione::app() -> axum::Router`.
async fn spawn_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app_no_db().await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    format!("http://{}", addr)
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/health", base))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from /api/v1/health"
    );

    let body: Value = resp.json().await.expect("body is not JSON");

    assert_eq!(
        body["status"], "ok",
        "health.status must be \"ok\", got: {}",
        body["status"]
    );

    let version = body["version"].as_str().unwrap_or("");
    assert!(
        !version.is_empty(),
        "health.version must be a non-empty string, got: {}",
        body["version"]
    );
}

// ---------------------------------------------------------------------------
// Chat — happy path (requires live Ollama; gated with #[ignore])
// ---------------------------------------------------------------------------

/// REASON: Ollama must be reachable at OLLAMA_BASE_URL. Run with:
///   OLLAMA_BASE_URL=http://localhost:11434 cargo test --test phase01_chat -- --ignored
#[tokio::test]
#[ignore]
async fn chat_endpoint_happy_path() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/chat", base))
        .json(&json!({ "prompt": "say pong" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/chat"
    );

    let body: Value = resp.json().await.expect("body is not JSON");

    let reply = body["reply"].as_str().unwrap_or("");
    assert!(
        !reply.is_empty(),
        "chat response must have a non-empty \"reply\" field, got: {}",
        body
    );

    let model = body["model"].as_str().unwrap_or("");
    assert!(
        !model.is_empty(),
        "chat response must have a non-empty \"model\" field, got: {}",
        body
    );
}

// ---------------------------------------------------------------------------
// Chat — missing `prompt` field must be rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_endpoint_rejects_missing_prompt() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/chat", base))
        .json(&json!({}))
        .send()
        .await
        .expect("request failed");

    // Contract allows 400 or 422 (axum extractor default is 422)
    assert!(
        resp.status().is_client_error(),
        "missing prompt must yield a 4xx status, got: {}",
        resp.status()
    );

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 422,
        "expected 400 or 422 for missing prompt, got: {}",
        status
    );
}

// ---------------------------------------------------------------------------
// Static assets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn static_index_served() {
    let base = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/", base))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK, "expected 200 from GET /");

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert!(
        content_type.starts_with("text/html"),
        "expected content-type starting with \"text/html\", got: \"{}\"",
        content_type
    );
}
