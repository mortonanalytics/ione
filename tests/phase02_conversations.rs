/// Phase 2 contract tests — Postgres-backed conversation history.
///
/// These tests are written against the contract in md/design/ione-v1-contract.md
/// and FAIL today (the implementation doesn't exist yet).  They are all
/// `#[ignore]`-gated and require a running Postgres instance.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Expected DATABASE_URL (default):
///   postgres://ione:ione@localhost:5433/ione
///
/// The sql-coder agent will create docker-compose.yml with a postgres:16+pgvector
/// service on port 5433.  To bring it up before running these tests:
///   docker compose up -d postgres
///
/// Run this suite:
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase02_conversations -- --ignored
///
/// ──────────────────────────────────────────────────────────────────────────
/// Required additions to Cargo.toml for this test file to compile:
///
/// [dependencies]  (also needed by the main crate)
///   sqlx  = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono", "json", "migrate"] }
///   uuid  = { version = "1", features = ["v4", "serde"] }
///   chrono = { version = "0.4", features = ["serde"] }
///   dotenvy = "0.15"
///
/// [dev-dependencies]  (test-only; reqwest and tokio already present)
///   (none beyond the above — sqlx and uuid are runtime deps that test code reuses)
///
/// ──────────────────────────────────────────────────────────────────────────
/// Expected `ione::app` signature after Phase 2 implementation:
///
///   pub async fn app(pool: sqlx::PgPool) -> axum::Router
///
/// The implementer MUST update `src/lib.rs` so that:
///   - `app(pool)` accepts a `PgPool` and wires it into `AppState`
///   - `main.rs` connects, runs migrations, and passes the pool to `app()`
///   - The old zero-argument `app()` is replaced (Phase 1 tests will need
///     their `spawn_app` updated to call `spawn_app_with_pool` or the pool
///     variant; the Phase 1 non-DB tests may alternatively keep a
///     `app_no_db()` shim, but the contract-required path is `app(pool)`)
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract field names (from ione-v1-contract.md):
///   Response (camelCase) : id, title, workspaceId, userId, createdAt,
///                          role, content, model, tokensIn, tokensOut,
///                          conversationId
///   DB columns (snake)   : id, title, workspace_id, user_id, created_at,
///                          role, content, model, tokens_in, tokens_out,
///                          conversation_id
///   Role enum values     : "user", "assistant", "system"
/// ──────────────────────────────────────────────────────────────────────────
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

// Default connection string; override via DATABASE_URL env var at runtime.
const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

/// Connect to Postgres, run migrations, truncate all Phase 2 tables so the
/// test starts from a clean slate, then boot the axum server on a random port.
///
/// Returns `(base_url, pool)` — tests can both make HTTP calls and introspect
/// the database directly.
///
/// Assumes `ione::app(pool)` is the Phase 2 signature.
async fn spawn_app() -> (String, PgPool) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is docker compose up -d postgres running?");

    // Run all pending migrations before each test.
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    // Clean slate: truncate all Phase 2 tables in FK-safe order.
    // RESTART IDENTITY resets serial sequences; CASCADE handles child rows.
    sqlx::query(
        "TRUNCATE organizations, users, conversations, messages
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    // Boot the server with the pool injected.
    // This call will FAIL TO COMPILE until `ione::app(pool)` is implemented.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

// ──────────────────────────────────────────────────────────────────────────────
// Bootstrap
// ──────────────────────────────────────────────────────────────────────────────

/// After `app(pool)` starts against a fresh DB the idempotent bootstrap must
/// have created exactly one organization row and one user with
/// email = 'default@localhost'.
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance.
#[tokio::test]
#[ignore]
async fn default_org_and_user_bootstrapped() {
    let (_base, pool) = spawn_app().await;

    let org_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM organizations")
        .fetch_one(&pool)
        .await
        .expect("org count query failed");

    assert_eq!(
        org_count, 1,
        "expected exactly one organization row after bootstrap, got {}",
        org_count
    );

    let user_row: (i64, String) =
        sqlx::query_as("SELECT COUNT(*), MAX(email) FROM users WHERE email = 'default@localhost'")
            .fetch_one(&pool)
            .await
            .expect("user query failed");

    assert_eq!(
        user_row.0, 1,
        "expected exactly one user with email='default@localhost', got {}",
        user_row.0
    );
    assert_eq!(
        user_row.1, "default@localhost",
        "default user email mismatch: got {:?}",
        user_row.1
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Conversations
// ──────────────────────────────────────────────────────────────────────────────

/// POST /api/v1/conversations with { "title": "hello" }
/// → 200, body has `id` (UUID), `title == "hello"`, `createdAt` (ISO8601).
/// Verify the row also exists in the `conversations` table.
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance.
#[tokio::test]
#[ignore]
async fn create_conversation_returns_conversation() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "hello" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/conversations, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body is not JSON");

    // id must be a valid UUID string
    let id_str = body["id"]
        .as_str()
        .expect("response must have an \"id\" string field");
    let id = Uuid::parse_str(id_str).expect("\"id\" field must be a valid UUID");

    // title must match what we sent
    assert_eq!(
        body["title"], "hello",
        "response \"title\" must be \"hello\", got: {}",
        body["title"]
    );

    // createdAt must be present and non-null
    assert!(
        !body["createdAt"].is_null(),
        "response must have a non-null \"createdAt\" field, got: {}",
        body
    );

    // Verify the row exists in the DB
    let db_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("db count query failed");

    assert_eq!(
        db_count, 1,
        "expected one conversations row with id={}, found {}",
        id, db_count
    );
}

/// Seed two conversations via API, then GET /api/v1/conversations
/// → 200, body.items is an array of length 2, ordered newest first (by createdAt).
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance.
#[tokio::test]
#[ignore]
async fn list_conversations_returns_items() {
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create first conversation
    client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "first" }))
        .send()
        .await
        .expect("create first request failed");

    // Small sleep so `created_at` ordering is deterministic across the two rows.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Create second conversation
    client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "second" }))
        .send()
        .await
        .expect("create second request failed");

    // List
    let resp = client
        .get(format!("{}/api/v1/conversations", base))
        .send()
        .await
        .expect("list request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/conversations, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body is not JSON");

    let items = body["items"]
        .as_array()
        .expect("response must have an \"items\" array");

    assert_eq!(
        items.len(),
        2,
        "expected 2 items in list response, got {}",
        items.len()
    );

    // Newest first: second conversation was created later, so it appears at index 0.
    assert_eq!(
        items[0]["title"], "second",
        "first item must be the newest conversation (\"second\"), got: {}",
        items[0]["title"]
    );
    assert_eq!(
        items[1]["title"], "first",
        "second item must be the oldest conversation (\"first\"), got: {}",
        items[1]["title"]
    );
}

/// Create a conversation, post two user messages via the messages endpoint,
/// then GET /api/v1/conversations/:id
/// → 200, body has `conversation` and `messages[]`.
/// Messages include both "user" and "assistant" roles, ordered by `createdAt`.
///
/// REASON: requires DATABASE_URL and Ollama (Ollama needed for assistant turn).
/// This test seeds messages through the HTTP endpoint so it is Ollama-gated;
/// use `post_message_persists_user_and_assistant_turns` for the full LLM path.
/// Here we POST two separate messages and verify ordering and role presence.
#[tokio::test]
#[ignore]
async fn get_conversation_with_messages_returns_ordered_history() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping Ollama-dependent history test");
        return;
    }
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a conversation
    let conv_resp: Value = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "history test" }))
        .send()
        .await
        .expect("create conversation failed")
        .json()
        .await
        .expect("create conversation body not JSON");

    let conv_id = conv_resp["id"].as_str().expect("conversation id missing");

    // Post first message (triggers user + assistant turns via Ollama)
    let msg1_resp: Value = client
        .post(format!(
            "{}/api/v1/conversations/{}/messages",
            base, conv_id
        ))
        .json(&json!({ "content": "message one" }))
        .send()
        .await
        .expect("post message 1 failed")
        .json()
        .await
        .expect("post message 1 body not JSON");

    // The response must be the assistant message
    assert_eq!(
        msg1_resp["role"], "assistant",
        "POST /messages must return the assistant Message, got role: {}",
        msg1_resp["role"]
    );

    // Post second message
    let _msg2_resp: Value = client
        .post(format!(
            "{}/api/v1/conversations/{}/messages",
            base, conv_id
        ))
        .json(&json!({ "content": "message two" }))
        .send()
        .await
        .expect("post message 2 failed")
        .json()
        .await
        .expect("post message 2 body not JSON");

    // Fetch the full conversation
    let get_resp = client
        .get(format!("{}/api/v1/conversations/{}", base, conv_id))
        .send()
        .await
        .expect("get conversation failed");

    assert_eq!(
        get_resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/conversations/:id, got {}",
        get_resp.status()
    );

    let body: Value = get_resp
        .json()
        .await
        .expect("get conversation body not JSON");

    // Top-level shape: { conversation, messages }
    assert!(
        !body["conversation"].is_null(),
        "response must have a \"conversation\" field"
    );

    let messages = body["messages"]
        .as_array()
        .expect("response must have a \"messages\" array");

    // 2 user turns + 2 assistant turns = 4 total
    assert_eq!(
        messages.len(),
        4,
        "expected 4 messages (2 user + 2 assistant), got {}",
        messages.len()
    );

    // Collect roles
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().expect("message must have role"))
        .collect();

    assert!(
        roles.contains(&"user"),
        "messages must include at least one \"user\" role, got: {:?}",
        roles
    );
    assert!(
        roles.contains(&"assistant"),
        "messages must include at least one \"assistant\" role, got: {:?}",
        roles
    );

    // Verify ordering: each message's createdAt >= previous
    for i in 1..messages.len() {
        let prev = messages[i - 1]["createdAt"]
            .as_str()
            .expect("message createdAt must be a string");
        let curr = messages[i]["createdAt"]
            .as_str()
            .expect("message createdAt must be a string");
        assert!(
            curr >= prev,
            "messages must be ordered by createdAt ascending; index {} ({}) < index {} ({})",
            i,
            curr,
            i - 1,
            prev
        );
    }

    // Each message must carry conversationId matching the conversation
    for msg in messages {
        assert_eq!(
            msg["conversationId"], conv_id,
            "every message must have conversationId matching the conversation"
        );
    }
}

/// POST /api/v1/conversations/:id/messages { "content": "say pong" }
/// → 200, response is the assistant Message (role=="assistant", non-empty content).
/// DB assertion: exactly two new messages rows for this conversation —
///   one with role='user' and content='say pong',
///   one with role='assistant' and non-empty content.
///
/// REASON: requires DATABASE_URL and live Ollama.
#[tokio::test]
#[ignore]
async fn post_message_persists_user_and_assistant_turns() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping Ollama-dependent post-message test");
        return;
    }
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a conversation
    let conv_body: Value = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "pong test" }))
        .send()
        .await
        .expect("create conversation failed")
        .json()
        .await
        .expect("create conversation body not JSON");

    let conv_id_str = conv_body["id"].as_str().expect("conversation id missing");
    let conv_id = Uuid::parse_str(conv_id_str).expect("conversation id must be a valid UUID");

    // Post one message
    let resp = client
        .post(format!(
            "{}/api/v1/conversations/{}/messages",
            base, conv_id_str
        ))
        .json(&json!({ "content": "say pong" }))
        .send()
        .await
        .expect("post message request failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/conversations/:id/messages, got {}",
        resp.status()
    );

    let msg_body: Value = resp.json().await.expect("message body not JSON");

    // Response must be the assistant reply
    assert_eq!(
        msg_body["role"], "assistant",
        "POST /messages response must have role=\"assistant\", got: {}",
        msg_body["role"]
    );

    let reply_content = msg_body["content"]
        .as_str()
        .expect("assistant message must have \"content\"");
    assert!(
        !reply_content.is_empty(),
        "assistant message content must not be empty"
    );

    // Response must have a valid id and conversationId
    let _assistant_id = Uuid::parse_str(
        msg_body["id"]
            .as_str()
            .expect("assistant message must have \"id\""),
    )
    .expect("assistant message id must be a valid UUID");

    assert_eq!(
        msg_body["conversationId"], conv_id_str,
        "assistant message conversationId must match the conversation"
    );

    // DB assertion: exactly 2 rows for this conversation
    let total_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
            .bind(conv_id)
            .fetch_one(&pool)
            .await
            .expect("total count query failed");

    assert_eq!(
        total_count, 2,
        "expected exactly 2 message rows (user + assistant) for conversation {}, found {}",
        conv_id, total_count
    );

    // User turn: role='user', content='say pong'
    let user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages
         WHERE conversation_id = $1
           AND role = 'user'
           AND content = 'say pong'",
    )
    .bind(conv_id)
    .fetch_one(&pool)
    .await
    .expect("user message count query failed");

    assert_eq!(
        user_count, 1,
        "expected one user message with content='say pong', found {}",
        user_count
    );

    // Assistant turn: role='assistant', content non-empty
    let assistant_content: String = sqlx::query_scalar(
        "SELECT content FROM messages
         WHERE conversation_id = $1
           AND role = 'assistant'
         LIMIT 1",
    )
    .bind(conv_id)
    .fetch_one(&pool)
    .await
    .expect("assistant message query failed");

    assert!(
        !assistant_content.is_empty(),
        "assistant message content in DB must not be empty"
    );
}

/// Create a conversation, post a message (DB insert directly bypassing HTTP to
/// avoid Ollama dependency), delete the conversation row via sqlx, assert all
/// child message rows are also gone (ON DELETE CASCADE).
///
/// REASON: requires DATABASE_URL pointing at a running Postgres instance.
#[tokio::test]
#[ignore]
async fn conversation_foreign_key_cascades_delete_messages() {
    let (base, pool) = spawn_app().await;
    let client = reqwest::Client::new();

    // Create a conversation via the API
    let conv_body: Value = client
        .post(format!("{}/api/v1/conversations", base))
        .json(&json!({ "title": "cascade test" }))
        .send()
        .await
        .expect("create conversation failed")
        .json()
        .await
        .expect("body not JSON");

    let conv_id =
        Uuid::parse_str(conv_body["id"].as_str().expect("missing id")).expect("invalid UUID");

    // Insert message rows directly into the DB to avoid Ollama dependency.
    sqlx::query(
        "INSERT INTO messages (conversation_id, role, content)
         VALUES ($1, 'user'::message_role, 'test message 1'),
                ($1, 'assistant'::message_role, 'test reply 1')",
    )
    .bind(conv_id)
    .execute(&pool)
    .await
    .expect("message insert failed");

    // Sanity: confirm 2 messages exist
    let before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
            .bind(conv_id)
            .fetch_one(&pool)
            .await
            .expect("count query failed");
    assert_eq!(
        before, 2,
        "expected 2 messages before delete, got {}",
        before
    );

    // Delete the conversation directly via sqlx
    sqlx::query("DELETE FROM conversations WHERE id = $1")
        .bind(conv_id)
        .execute(&pool)
        .await
        .expect("conversation delete failed");

    // Messages must be gone due to ON DELETE CASCADE
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
        .bind(conv_id)
        .fetch_one(&pool)
        .await
        .expect("count query after delete failed");

    assert_eq!(
        after, 0,
        "expected 0 messages after conversation deleted (cascade), found {}",
        after
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Phase 1 regression: stateless chat endpoint must still work
// ──────────────────────────────────────────────────────────────────────────────

/// The stateless POST /api/v1/chat endpoint from Phase 1 must be preserved
/// as-is after Phase 2 changes.
///
/// REASON: requires DATABASE_URL and live Ollama.
#[tokio::test]
#[ignore]
async fn phase_1_chat_endpoint_still_works() {
    if std::env::var("IONE_SKIP_LIVE").is_ok() {
        eprintln!("IONE_SKIP_LIVE set — skipping Ollama-dependent chat regression");
        return;
    }
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/chat", base))
        .json(&json!({ "prompt": "ping" }))
        .send()
        .await
        .expect("POST /api/v1/chat failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from POST /api/v1/chat (Phase 1 regression), got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body not JSON");

    let reply = body["reply"].as_str().unwrap_or("");
    assert!(
        !reply.is_empty(),
        "chat response must have a non-empty \"reply\" field after Phase 2, got: {}",
        body
    );

    let model = body["model"].as_str().unwrap_or("");
    assert!(
        !model.is_empty(),
        "chat response must have a non-empty \"model\" field after Phase 2, got: {}",
        body
    );
}
