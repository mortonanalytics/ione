/// Executable conformance tests for the frozen v1 app-integration contract.
///
/// Target: md/design/app-integration-contract-v1.md (v1, frozen 2026-07-25)
///
/// IONe is itself an MCP peer, so the surfaces it serves can be asserted against
/// the same contract third-party peers must satisfy. Every assertion below cites
/// the contract section it freezes.
///
/// Prerequisites:
///   docker compose up -d postgres
///
/// Run:
///   SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
///     DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test contract_v1_integration -- --ignored --test-threads=1
///
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
use std::net::SocketAddr;

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_STATIC_BEARER: &str = "contract-v1-test-bearer";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

// ─── Harness ──────────────────────────────────────────────────────────────────

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_OAUTH_STATIC_BEARER", TEST_STATIC_BEARER);
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migration failed");

    sqlx::query(
        "TRUNCATE pending_peer_tool_calls, webhook_events_seen, workspace_peer_bindings,
                  audit_events, approvals, artifacts,
                  peers, trust_issuers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let app = ione::app(pool.clone()).await;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });
    (format!("http://{}", addr), pool)
}

// ─── MCP helpers ──────────────────────────────────────────────────────────────

/// Establish an MCP session and return its id, per contract §1 "Session recovery".
async fn mcp_session_id(client: &reqwest::Client, base: &str) -> Option<String> {
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Authorization", format!("Bearer {TEST_STATIC_BEARER}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": "session",
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {} }
        }))
        .send()
        .await
        .expect("initialize failed");
    if !resp.status().is_success() {
        return None;
    }
    let header_session = resp
        .headers()
        .get("MCP-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body: Value = resp.json().await.expect("initialize response not JSON");
    header_session.or_else(|| {
        body["result"]["sessionId"]
            .as_str()
            .map(str::to_string)
            .or_else(|| body["result"]["session_id"].as_str().map(str::to_string))
    })
}

/// POST a JSON-RPC request to IONe's own `/mcp`, presenting credentials exactly
/// as contract §1 requires: `Authorization: Bearer <token>`.
async fn mcp_post(base: &str, body: Value) -> Value {
    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{base}/mcp"))
        .header("Authorization", format!("Bearer {TEST_STATIC_BEARER}"))
        .json(&body);
    if let Some(session_id) = mcp_session_id(&client, base).await {
        req = req.header("MCP-Session-Id", session_id);
    }
    let resp = req.send().await.expect("POST /mcp failed");
    assert!(
        resp.status().is_success(),
        "POST /mcp returned {}",
        resp.status()
    );
    resp.json().await.expect("/mcp response not JSON")
}

// ─── Webhook helpers ──────────────────────────────────────────────────────────

async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org")
}

async fn default_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("workspace")
}

async fn insert_active_peer(pool: &PgPool, org_id: Uuid) -> Uuid {
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'https://issuer-contract-v1.test', 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .fetch_one(pool)
    .await
    .expect("issuer");

    sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, status)
         VALUES ($1, 'contract-v1-peer', 'https://peer-contract-v1.test/mcp', $2,
                 '{}'::jsonb, 'active'::peer_status)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_id)
    .fetch_one(pool)
    .await
    .expect("peer")
}

async fn provision_secret(base: &str, peer_id: Uuid) -> String {
    let body: Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/webhook/provision"))
        .send()
        .await
        .expect("provision request")
        .json()
        .await
        .expect("provision json");
    body["signingSecret"]
        .as_str()
        .expect("signingSecret")
        .to_string()
}

/// Build `X-IONe-Signature` exactly as contract §3.1 specifies:
/// HMAC-SHA256 over `t_ascii ++ b"." ++ raw_body`, hex-encoded.
fn sign(secret: &str, body: &str, ts: i64) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// A contract §3.3 envelope with every required field present.
fn envelope(peer_id: Uuid, event_id: &str, tenant: &str) -> Value {
    json!({
        "id": event_id,
        "type": "alert.created",
        "occurred_at": Utc::now(),
        "peer_id": peer_id,
        "foreign_tenant_id": tenant,
        "severity": "routine",
        "data": { "message": "contract v1 conformance" },
        "approval_required": false
    })
}

async fn insert_binding(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid, tenant: &str) {
    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, $3, 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .bind(tenant)
    .execute(pool)
    .await
    .expect("binding");
}

// ─── §6 — whoami:// ───────────────────────────────────────────────────────────

/// Contract §6: `whoami://` returns the frozen mimeType and all seven documented
/// fields. All seven keys must be present; a value may be null but a key may not
/// be missing.
#[tokio::test]
#[ignore]
async fn whoami_resource_matches_frozen_v1_shape() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;

    let body = mcp_post(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": "whoami://" }
        }),
    )
    .await;

    let content = &body["result"]["contents"][0];
    assert_eq!(
        content["uri"], "whoami://",
        "§6: contents[0].uri must echo the requested URI"
    );
    assert_eq!(
        content["mimeType"], "application/vnd.ione.whoami+json",
        "§6: whoami mimeType is frozen in v1"
    );

    // §6: contents[0].text is a JSON *string* whose parsed value is the payload.
    let text = content["text"]
        .as_str()
        .expect("§6: contents[0].text must be a JSON string");
    let payload: Value =
        serde_json::from_str(text).expect("§6: contents[0].text must parse as JSON");
    let obj = payload
        .as_object()
        .expect("§6: whoami payload must be an object");

    for field in [
        "peer_id",
        "foreign_tenant_id",
        "foreign_tenant_name",
        "foreign_workspace_id",
        "foreign_user_id",
        "foreign_user_email",
        "foreign_roles",
    ] {
        assert!(
            obj.contains_key(field),
            "§6: whoami payload is missing required key '{field}'"
        );
    }

    // §6: IONe's own values table.
    assert_eq!(
        payload["peer_id"], "ione",
        "§6: peer_id defaults to \"ione\" when IONE_BIND is unset"
    );
    assert_eq!(
        payload["foreign_tenant_id"],
        json!(org_id.to_string()),
        "§6: IONe reports the caller's org_id as foreign_tenant_id"
    );
    assert!(
        payload["foreign_user_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .is_some(),
        "§6: foreign_user_id must be a UUID string"
    );
    assert!(
        payload["foreign_roles"].is_array(),
        "§6: foreign_roles must be an array (possibly empty)"
    );
    assert!(
        payload["foreign_workspace_id"].is_null(),
        "§6: IONe's own foreign_workspace_id is always null"
    );
}

// ─── §5.4 — slice:// is NOT served by IONe ────────────────────────────────────

/// Contract §5.4 (and Appendix A divergence #1): IONe's own `/mcp` advertises
/// exactly one resource, `whoami://`, and rejects `slice://`. `slice://` is a
/// peer→IONe contract only; there is no aggregated slice on IONe's own surface.
#[tokio::test]
#[ignore]
async fn ione_advertises_only_whoami_and_does_not_serve_slice() {
    let (base, _pool) = spawn_app().await;

    let listed = mcp_post(
        &base,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list", "params": {} }),
    )
    .await;
    let resources = listed["result"]["resources"]
        .as_array()
        .expect("§5.4: resources/list must return a resources array");
    assert_eq!(
        resources.len(),
        1,
        "§5.4: IONe advertises exactly one resource; got {resources:?}"
    );
    assert_eq!(resources[0]["uri"], "whoami://");
    assert_eq!(
        resources[0]["mimeType"], "application/vnd.ione.whoami+json",
        "§6: the advertised whoami mimeType is frozen"
    );
    assert!(
        !resources.iter().any(|r| r["uri"] == "slice://"),
        "§5.4: IONe must not advertise slice:// on its own surface"
    );

    // §5.4: reading slice:// from IONe is a JSON-RPC error, not a payload.
    let slice = mcp_post(
        &base,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/read",
            "params": { "uri": "slice://" }
        }),
    )
    .await;
    assert_eq!(
        slice["error"]["code"], -32602,
        "§5.4: reading slice:// from IONe must return JSON-RPC -32602, got {slice}"
    );
    assert!(
        slice["result"].is_null(),
        "§5.4: an errored resources/read must not carry a result"
    );
}

// ─── §3.1 — webhook signature scheme ──────────────────────────────────────────

/// Contract §3.1: a correctly-signed webhook is accepted; the same request with a
/// tampered `v1=` digest is rejected 401 `webhook_unauthorized`. This is the
/// authentication property the whole push surface rests on — the digest, not the
/// path `peer_id`, is what authenticates the sender.
#[tokio::test]
#[ignore]
async fn webhook_accepts_valid_signature_and_rejects_tampered_digest() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let workspace_id = default_workspace_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id).await;
    let secret = provision_secret(&base, peer_id).await;
    insert_binding(&pool, workspace_id, peer_id, "t-contract-v1").await;

    let client = reqwest::Client::new();

    // Positive: a correctly-signed, fully-populated §3.3 envelope is accepted.
    let body = serde_json::to_string(&envelope(peer_id, "evt-contract-ok", "t-contract-v1"))
        .expect("body");
    let accepted = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            sign(&secret, &body, Utc::now().timestamp()),
        )
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(
        accepted.status(),
        StatusCode::OK,
        "§3.1: a correctly-signed webhook must be accepted"
    );
    let ack: Value = accepted.json().await.expect("ack json");
    assert_eq!(ack["ok"], true, "§3.5: ack carries ok=true");
    assert_eq!(
        ack["duplicate"], false,
        "§3.5: first delivery is not a replay"
    );

    // Negative: flip one hex nibble of the digest, keeping `t` and the body byte
    // -identical. Only the HMAC changes, so this isolates digest verification.
    let ts = Utc::now().timestamp();
    let good = sign(&secret, &body, ts);
    let (prefix, digest) = good.split_once("v1=").expect("v1= in signature");
    let mut chars: Vec<char> = digest.chars().collect();
    chars[0] = if chars[0] == '0' { '1' } else { '0' };
    let tampered: String = chars.into_iter().collect();
    assert_eq!(
        tampered.len(),
        64,
        "§3.1: the tampered digest must remain exactly 64 hex chars so the \
         length check cannot be what rejects it"
    );
    assert_ne!(tampered, digest, "the digest must actually differ");

    let rejected = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", format!("{prefix}v1={tampered}"))
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(
        rejected.status(),
        StatusCode::UNAUTHORIZED,
        "§3.1: a tampered v1 digest must be rejected 401"
    );
    let err: Value = rejected.json().await.expect("error json");
    assert_eq!(
        err["error"], "webhook_unauthorized",
        "§3.5: signature failure reports webhook_unauthorized"
    );
}

// ─── §7.2 — non-leaky webhook error envelope ──────────────────────────────────

/// Contract §7.2: webhook error bodies carry ONLY `error` — no `message`, `hint`,
/// or `field`. The endpoint is unauthenticated, so a descriptive error would let
/// an anonymous caller distinguish peer-missing from peer-revoked from
/// bad-signature and enumerate peers/tenants.
#[tokio::test]
#[ignore]
async fn webhook_error_envelope_is_non_leaky() {
    let (base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;
    let peer_id = insert_active_peer(&pool, org_id).await;
    let secret = provision_secret(&base, peer_id).await;
    let client = reqwest::Client::new();

    // 401 path: unknown peer — signature cannot possibly verify.
    let unknown_peer = Uuid::new_v4();
    let body = serde_json::to_string(&envelope(unknown_peer, "evt-unknown", "t-contract-v1"))
        .expect("body");
    let resp = client
        .post(format!("{base}/webhooks/peer/{unknown_peer}"))
        .header(
            "X-IONe-Signature",
            sign(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_non_leaky(resp.json().await.expect("json"), "webhook_unauthorized");

    // 400 path: valid signature, active peer, but no active binding for the
    // tenant (§3.4). A different failure class must produce the same shape.
    let body =
        serde_json::to_string(&envelope(peer_id, "evt-no-binding", "t-unbound")).expect("body");
    let resp = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            sign(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_non_leaky(resp.json().await.expect("json"), "webhook_rejected");
}

fn assert_non_leaky(body: Value, expected_kind: &str) {
    let obj = body
        .as_object()
        .expect("§7.2: webhook error body must be an object");
    assert_eq!(
        obj.get("error").and_then(Value::as_str),
        Some(expected_kind),
        "§7.2: error discriminator mismatch in {body}"
    );
    for leak in ["message", "hint", "field"] {
        assert!(
            !obj.contains_key(leak),
            "§7.2: webhook error must not expose '{leak}' — it turns the \
             unauthenticated endpoint into an enumeration oracle: {body}"
        );
    }
    assert_eq!(
        obj.len(),
        1,
        "§7.2: webhook error body carries exactly one key: {body}"
    );
}

// ─── §7.1 — global error envelope ─────────────────────────────────────────────

/// Contract §7.1: every non-webhook 4xx uses `{error, message, hint?, field?}`
/// with a snake_case `error` discriminator clients branch on.
#[tokio::test]
#[ignore]
async fn global_error_envelope_shape_on_representative_4xx() {
    let (base, _pool) = spawn_app().await;

    let missing_peer = Uuid::new_v4();
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/peers/{missing_peer}/webhook/provision"
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "§7.1: provisioning an unknown peer is a 404"
    );

    let body: Value = resp.json().await.expect("json");
    let obj = body
        .as_object()
        .expect("§7.1: error body must be an object");

    let kind = obj
        .get("error")
        .and_then(Value::as_str)
        .expect("§7.1: 'error' is required and must be a string");
    assert_eq!(kind, "not_found", "§7.1: discriminator is stable");
    assert!(
        kind.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "§7.1: 'error' must be snake_case, got '{kind}'"
    );

    assert!(
        obj.get("message").and_then(Value::as_str).is_some(),
        "§7.1: non-webhook errors carry a user-facing 'message': {body}"
    );
    // `hint` and `field` are optional, but must be strings when present.
    for optional in ["hint", "field"] {
        if let Some(value) = obj.get(optional) {
            assert!(
                value.is_string(),
                "§7.1: '{optional}' must be a string when present: {body}"
            );
        }
    }
}
