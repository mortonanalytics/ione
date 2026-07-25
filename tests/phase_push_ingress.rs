use std::net::SocketAddr;

use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const TEST_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

async fn spawn_app() -> (String, PgPool) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

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

    sqlx::query(
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
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

async fn spawn_state() -> (PgPool, ione::state::AppState) {
    std::env::set_var("IONE_AUTH_MODE", "local");
    std::env::set_var("IONE_TOKEN_KEY", TEST_KEY);
    std::env::set_var("IONE_WEBHOOK_SECRET_KEY", TEST_KEY);

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
    sqlx::query(
        "TRUNCATE webhook_events_seen, workspace_peer_bindings, audit_events, approvals, artifacts,
                  trust_issuers, peers, routing_decisions, survivors, signals,
                  stream_events, streams, connectors,
                  memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed");
    let (_app, state) = ione::app_with_state(pool.clone()).await;
    (pool, state)
}

async fn default_ids(pool: &PgPool) -> (Uuid, Uuid) {
    let org_id = sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org");
    let workspace_id = sqlx::query_scalar("SELECT id FROM workspaces LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("workspace");
    (org_id, workspace_id)
}

async fn insert_peer(pool: &PgPool, org_id: Uuid, status: &str) -> Uuid {
    let issuer_id: Uuid = sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, 'aud', 'secret:test', '{}'::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("https://issuer-{status}.test"))
    .fetch_one(pool)
    .await
    .expect("issuer");
    sqlx::query_scalar(
        "INSERT INTO peers (org_id, name, mcp_url, issuer_id, sharing_policy, status)
         VALUES ($1, $2, $3, $4, '{}'::jsonb, $5::peer_status)
         RETURNING id",
    )
    .bind(org_id)
    .bind(format!("peer-{status}"))
    .bind(format!("https://peer-{status}.test/mcp"))
    .bind(issuer_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("peer")
}

async fn provision(base: &str, peer_id: Uuid) -> String {
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

fn signed_headers(secret: &str, body: &str, ts: i64) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let digest = hex::encode(mac.finalize().into_bytes());
    format!("t={ts},v1={digest}")
}

fn envelope(peer_id: Uuid, event_id: &str, tenant: &str) -> Value {
    json!({
        "id": event_id,
        "type": "alert.created",
        "occurred_at": Utc::now(),
        "peer_id": peer_id,
        "foreign_tenant_id": tenant,
        "severity": "routine",
        "data": { "message": "hello" },
        "approval_required": false
    })
}

async fn bind_tenant(pool: &PgPool, workspace_id: Uuid, peer_id: Uuid, tenant: &str) {
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

/// An active peer with a provisioned secret and an active `t-acme` binding, so a
/// conforming envelope is a 200. Every rejection asserted against this fixture is
/// therefore attributable to the rule under test rather than to a missing binding.
async fn setup_bound_peer() -> (String, PgPool, Uuid, Uuid, String) {
    let (base, pool) = spawn_app().await;
    let (org_id, workspace_id) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;
    bind_tenant(&pool, workspace_id, peer_id, "t-acme").await;
    (base, pool, workspace_id, peer_id, secret)
}

async fn post_event(base: &str, peer_id: Uuid, secret: &str, event: &Value) -> reqwest::Response {
    let body = serde_json::to_string(event).expect("body");
    let ts = Utc::now().timestamp();
    reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", signed_headers(secret, &body, ts))
        .body(body)
        .send()
        .await
        .expect("post")
}

#[tokio::test]
#[ignore]
async fn provision_returns_secret_and_stores_ciphertext() {
    let (base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/peers/{peer_id}/webhook/provision"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert!(body["signingSecret"].as_str().unwrap_or("").len() >= 64);
    assert!(body["webhookUrl"]
        .as_str()
        .unwrap_or("")
        .ends_with(&format!("/webhooks/peer/{peer_id}")));

    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT webhook_secret_ciphertext FROM peers WHERE id = $1")
            .bind(peer_id)
            .fetch_one(&pool)
            .await
            .expect("ciphertext");
    assert!(stored.is_some());
}

#[tokio::test]
#[ignore]
async fn valid_event_replays_and_no_binding_do_not_poison_dedup() {
    let (base, pool) = spawn_app().await;
    let (org_id, workspace_id) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;

    let event = envelope(peer_id, "evt-1", "t-acme");
    let body = serde_json::to_string(&event).expect("body");
    let sig = signed_headers(&secret, &body, Utc::now().timestamp());
    let client = reqwest::Client::new();
    let no_binding = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", &sig)
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(no_binding.status(), StatusCode::BAD_REQUEST);
    let seen: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events_seen WHERE event_id = 'evt-1'")
            .fetch_one(&pool)
            .await
            .expect("seen count");
    assert_eq!(seen, 0);

    sqlx::query(
        "INSERT INTO workspace_peer_bindings (workspace_id, peer_id, foreign_tenant_id, status)
         VALUES ($1, $2, 't-acme', 'active'::binding_status)",
    )
    .bind(workspace_id)
    .bind(peer_id)
    .execute(&pool)
    .await
    .expect("binding");

    let accepted = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted_body: Value = accepted.json().await.expect("json");
    assert_eq!(accepted_body["duplicate"], false);

    let replay = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    let replay_body: Value = replay.json().await.expect("json");
    assert_eq!(replay_body["duplicate"], true);
}

#[tokio::test]
#[ignore]
async fn stale_invalid_and_revoked_webhooks_are_rejected() {
    let (base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;
    let mut event = envelope(peer_id, "evt-invalid", "t-acme");
    event["occurred_at"] = json!(Utc::now() - Duration::minutes(10));
    let body = serde_json::to_string(&event).expect("body");
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    sqlx::query("UPDATE peers SET status = 'revoked'::peer_status WHERE id = $1")
        .bind(peer_id)
        .execute(&pool)
        .await
        .expect("revoke peer");
    let body = serde_json::to_string(&envelope(peer_id, "evt-revoked", "t-acme")).expect("body");
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header(
            "X-IONe-Signature",
            signed_headers(&secret, &body, Utc::now().timestamp()),
        )
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn delivery_pass_creates_foreign_tenant_approval_for_gated_signal() {
    let (pool, state) = spawn_state().await;
    let (_org_id, workspace_id) = default_ids(&pool).await;
    let signal_id: Uuid = sqlx::query_scalar(
        "INSERT INTO signals
           (workspace_id, source, title, body, evidence, severity, approval_required)
         VALUES ($1, 'connector_event'::signal_source, 'Webhook command', 'Body',
                 $2, 'command'::severity, true)
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(json!({ "foreign_tenant_id": "t-acme" }))
    .fetch_one(&pool)
    .await
    .expect("signal");
    let survivor_id: Uuid = sqlx::query_scalar(
        "INSERT INTO survivors (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
         VALUES ($1, 'test', 'survive'::critic_verdict, 'ok', 1.0, '[]'::jsonb)
         RETURNING id",
    )
    .bind(signal_id)
    .fetch_one(&pool)
    .await
    .expect("survivor");
    sqlx::query(
        "INSERT INTO routing_decisions
           (survivor_id, target_kind, target_ref, classifier_model, rationale)
         VALUES ($1, 'draft'::routing_target, '{}'::jsonb, 'test', 'forced')",
    )
    .bind(survivor_id)
    .execute(&pool)
    .await
    .expect("routing decision");

    ione::services::scheduler::run_tick(&state, true)
        .await
        .expect("scheduler tick");

    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT ap.foreign_tenant_id
         FROM approvals ap
         JOIN artifacts art ON art.id = ap.artifact_id
         WHERE art.workspace_id = $1
         LIMIT 1",
    )
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("approval");
    assert_eq!(tenant.as_deref(), Some("t-acme"));
}

/// Contract §3.1 header grammar: `X-IONe-Signature` is a comma-separated list of
/// `key=value` pairs in which only `t` and `v1` are permitted, each at most once,
/// with `v1` exactly 64 lowercase hex characters. Every rejection branch of
/// `parse_signature` (src/routes/webhooks.rs:164-196) is covered here; the
/// tampered-but-well-formed digest case lives in `contract_v1_integration.rs`.
///
/// Note the status is 400 `webhook_rejected`, not 401: a header that cannot be
/// parsed never reaches `verify_signature`.
#[tokio::test]
#[ignore]
async fn malformed_signature_headers_are_rejected() {
    let (base, _pool, _workspace_id, peer_id, secret) = setup_bound_peer().await;

    // A body and its genuinely valid digest, so each variant below differs from a
    // accepted request *only* in the header grammar.
    let body = serde_json::to_string(&envelope(peer_id, "evt-sig", "t-acme")).expect("body");
    let ts = Utc::now().timestamp();
    let digest = signed_headers(&secret, &body, ts)
        .split("v1=")
        .nth(1)
        .expect("digest")
        .to_string();
    assert_eq!(digest.len(), 64, "fixture digest must be 64 hex chars");

    let cases: Vec<(&str, String)> = vec![
        ("duplicate t key", format!("t={ts},t={ts},v1={digest}")),
        (
            "duplicate v1 key",
            format!("t={ts},v1={digest},v1={digest}"),
        ),
        ("unknown key", format!("t={ts},v1={digest},alg=sha256")),
        ("missing t", format!("v1={digest}")),
        ("missing v1", format!("t={ts}")),
        ("non-hex digest", format!("t={ts},v1={}", "z".repeat(64))),
        ("digest too short", format!("t={ts},v1={}", &digest[..63])),
        ("digest too long", format!("t={ts},v1={digest}00")),
        ("pair without '='", format!("t={ts},v1")),
        ("non-integer t", format!("t=not-a-number,v1={digest}")),
    ];

    let client = reqwest::Client::new();
    for (label, header) in cases {
        let resp = client
            .post(format!("{base}/webhooks/peer/{peer_id}"))
            .header("X-IONe-Signature", &header)
            .body(body.clone())
            .send()
            .await
            .expect("post");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "§3.1: signature header '{label}' must be rejected"
        );
        let err: Value = resp.json().await.expect("json");
        assert_eq!(err["error"], "webhook_rejected", "case '{label}'");
    }

    // A request with no signature header at all is likewise rejected.
    let unsigned = client
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(unsigned.status(), StatusCode::BAD_REQUEST);

    // Control: the same fixture with a well-formed header is accepted, proving the
    // rejections above are caused by the header grammar and nothing else.
    let ok = post_event(
        &base,
        peer_id,
        &secret,
        &envelope(peer_id, "evt-sig-ok", "t-acme"),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK, "control must be accepted");
}

/// Contract §3.2 defines two independent windows. `stale_invalid_and_revoked_webhooks_are_rejected`
/// moves `occurred_at` ten minutes, which violates both at once. This pins the
/// `abs(t - occurred_at) <= 30` rule *in isolation*: `t` is fresh (well inside the
/// 300 s header window) while `occurred_at` sits 120 s away from it.
#[tokio::test]
#[ignore]
async fn header_event_skew_is_enforced_independently_of_header_freshness() {
    let (base, _pool, _workspace_id, peer_id, secret) = setup_bound_peer().await;

    let mut event = envelope(peer_id, "evt-skew", "t-acme");
    event["occurred_at"] = json!(Utc::now() - Duration::seconds(120));
    let body = serde_json::to_string(&event).expect("body");
    let ts = Utc::now().timestamp();
    // The header freshness rule is satisfied, so only the 30 s skew rule can fire.
    assert!(
        (Utc::now().timestamp() - ts).abs() <= 300,
        "fixture must satisfy abs(now - t) <= 300"
    );
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", signed_headers(&secret, &body, ts))
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "§3.2: abs(t - occurred_at) > 30 must be rejected even when t is fresh"
    );

    // Control: the same event inside the 30 s skew window is accepted.
    let mut fresh = envelope(peer_id, "evt-skew-ok", "t-acme");
    fresh["occurred_at"] = json!(Utc::now() - Duration::seconds(5));
    let ok = post_event(&base, peer_id, &secret, &fresh).await;
    assert_eq!(ok.status(), StatusCode::OK, "control must be accepted");
}

/// Contract §3.2: dedup rows are retained 72 h. `cleanup_expired`
/// (src/repos/webhook_event_repo.rs:32-42) runs from the scheduler; this pins the
/// boundary so the purge cannot silently start deleting live dedup state.
#[tokio::test]
#[ignore]
async fn expired_dedup_rows_are_purged_after_72_hours() {
    let (_base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;

    for (event_id, age_hours) in [("evt-stale", 73), ("evt-fresh", 71)] {
        sqlx::query(
            "INSERT INTO webhook_events_seen (event_id, peer_id, received_at)
             VALUES ($1, $2, now() - make_interval(hours => $3))",
        )
        .bind(event_id)
        .bind(peer_id)
        .bind(age_hours)
        .execute(&pool)
        .await
        .expect("seed dedup row");
    }

    let deleted = ione::repos::WebhookEventRepo::new(pool.clone())
        .cleanup_expired()
        .await
        .expect("cleanup_expired");
    assert_eq!(deleted, 1, "only the row older than 72 h may be purged");

    let survivors: Vec<String> =
        sqlx::query_scalar("SELECT event_id FROM webhook_events_seen ORDER BY event_id")
            .fetch_all(&pool)
            .await
            .expect("survivors");
    assert_eq!(
        survivors,
        vec!["evt-fresh".to_string()],
        "the row inside the 72 h window must survive the purge"
    );
}

/// Contract §3.3 envelope schema. Each case is correctly signed and targets a peer
/// with an active binding, so a 400 is attributable to `validate_envelope`
/// (src/routes/webhooks.rs:224-255) rather than to a missing binding or a bad
/// signature. The oversized-tenant case gets its own matching binding for the same
/// reason.
#[tokio::test]
#[ignore]
async fn envelope_validation_rejects_malformed_fields() {
    let (base, pool, _workspace_id, peer_id, secret) = setup_bound_peer().await;
    let (org_id, _) = default_ids(&pool).await;

    // `wpb_unique_workspace_peer` allows one binding per (workspace, peer), so the
    // two length-boundary tenants get their own workspaces. With bindings in place,
    // neither case can be rejected for want of a binding.
    let long_tenant = "x".repeat(513);
    let bounded_tenant = "x".repeat(512);
    for tenant in [&long_tenant, &bounded_tenant] {
        let ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspaces (org_id, name) VALUES ($1, $2) RETURNING id",
        )
        .bind(org_id)
        .bind(format!("ws-tenant-{}", tenant.len()))
        .fetch_one(&pool)
        .await
        .expect("workspace");
        bind_tenant(&pool, ws, peer_id, tenant).await;
    }

    let mut cases: Vec<(&str, Value)> = Vec::new();

    let mut wrong_peer = envelope(peer_id, "evt-peer-mismatch", "t-acme");
    wrong_peer["peer_id"] = json!(Uuid::new_v4());
    cases.push(("body peer_id != path peer_id", wrong_peer));

    let mut long_id = envelope(peer_id, "evt-long-id", "t-acme");
    long_id["id"] = json!("x".repeat(256));
    cases.push(("id longer than 255", long_id));

    let mut empty_id = envelope(peer_id, "evt-empty-id", "t-acme");
    empty_id["id"] = json!("");
    cases.push(("empty id", empty_id));

    cases.push((
        "foreign_tenant_id longer than 512",
        envelope(peer_id, "evt-long-tenant", &long_tenant),
    ));

    let mut empty_tenant = envelope(peer_id, "evt-empty-tenant", "t-acme");
    empty_tenant["foreign_tenant_id"] = json!("");
    cases.push(("empty foreign_tenant_id", empty_tenant));

    for (label, bad_type) in [
        ("uppercase type", "Alert.Created"),
        ("type with a space", "alert created"),
        ("type with a colon", "alert:created"),
        ("empty type", ""),
    ] {
        let mut event = envelope(peer_id, &format!("evt-type-{label}"), "t-acme");
        event["type"] = json!(bad_type);
        cases.push((label, event));
    }

    for (label, bad_data) in [
        ("data is an array", json!([1, 2, 3])),
        ("data is a string", json!("not an object")),
        ("data is a number", json!(7)),
        ("data is null", Value::Null),
    ] {
        let mut event = envelope(peer_id, &format!("evt-data-{label}"), "t-acme");
        event["data"] = bad_data;
        cases.push((label, event));
    }

    for (label, event) in cases {
        let resp = post_event(&base, peer_id, &secret, &event).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "§3.3: envelope with '{label}' must be rejected"
        );
        let err: Value = resp.json().await.expect("json");
        assert_eq!(err["error"], "webhook_rejected", "case '{label}'");
    }

    // Control: a 512-char tenant (the inclusive upper bound) is accepted, proving the
    // 513-char rejection above is the length rule and not a missing binding.
    let ok = post_event(
        &base,
        peer_id,
        &secret,
        &envelope(peer_id, "evt-bounded-tenant", &bounded_tenant),
    )
    .await;
    assert_eq!(
        ok.status(),
        StatusCode::OK,
        "a 512-char foreign_tenant_id is within the contract bound"
    );
}

/// Contract §3.3 freezes `approval_required` as **required with no default** —
/// omitting it is a deserialization failure ⇒ 400 (see also Appendix A, divergence
/// #2). This pins that, and pins the policy floor at §3.3: the flag may escalate
/// but never de-escalate, so `flagged`/`command` are gated regardless of the flag.
#[tokio::test]
#[ignore]
async fn approval_required_is_mandatory_and_severity_escalates_the_policy_floor() {
    let (base, pool, _workspace_id, peer_id, secret) = setup_bound_peer().await;

    // §3.3: the field is required. An envelope omitting it is rejected.
    let mut missing = envelope(peer_id, "evt-no-flag", "t-acme");
    missing
        .as_object_mut()
        .expect("object")
        .remove("approval_required");
    let resp = post_event(&base, peer_id, &secret, &missing).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "§3.3: omitting approval_required is a deserialization failure"
    );
    let seen: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events_seen WHERE event_id = 'evt-no-flag'",
    )
    .fetch_one(&pool)
    .await
    .expect("seen count");
    assert_eq!(seen, 0, "a rejected envelope must not poison dedup");

    // The policy floor: severity escalates even when the peer sends `false`, and a
    // routine event with `false` is not spuriously escalated.
    for (event_id, severity, flag, expected) in [
        ("evt-floor-flagged", "flagged", false, true),
        ("evt-floor-command", "command", false, true),
        ("evt-floor-routine", "routine", false, false),
        ("evt-floor-optin", "routine", true, true),
    ] {
        let mut event = envelope(peer_id, event_id, "t-acme");
        event["severity"] = json!(severity);
        event["approval_required"] = json!(flag);
        let resp = post_event(&base, peer_id, &secret, &event).await;
        assert_eq!(resp.status(), StatusCode::OK, "case '{event_id}'");

        let stored: bool = sqlx::query_scalar(
            "SELECT approval_required FROM signals WHERE evidence->>'event_id' = $1",
        )
        .bind(event_id)
        .fetch_one(&pool)
        .await
        .expect("signal");
        assert_eq!(
            stored, expected,
            "§3.3 policy floor: severity '{severity}' with approval_required={flag} \
             must store approval_required={expected}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn oversized_body_is_rejected_before_handling() {
    // AC-9: the 256 KB DefaultBodyLimit must reject before the handler reads the
    // body. Regression guard for the unauthenticated-endpoint DoS control.
    let (base, pool) = spawn_app().await;
    let (org_id, _) = default_ids(&pool).await;
    let peer_id = insert_peer(&pool, org_id, "active").await;
    let secret = provision(&base, peer_id).await;

    let big = "x".repeat(300 * 1024); // 300 KB > 256 KB cap
    let ts = Utc::now().timestamp();
    let resp = reqwest::Client::new()
        .post(format!("{base}/webhooks/peer/{peer_id}"))
        .header("X-IONe-Signature", signed_headers(&secret, &big, ts))
        .body(big)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
