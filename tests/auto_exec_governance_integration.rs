/// Auto-exec governance integration tests (design `md/design/auto-exec-governance.md`).
///
/// Covers AC-3a/3b/3c (CRUD round-trip), AC-4 (management gate), AC-5 (Guard A
/// authorship escalation), AC-7 (write-time fail-closed validation), AC-8
/// (severity_cap default), AC-10 (policy audit trail).
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test auto_exec_governance_integration -- --ignored --test-threads=1
use std::net::SocketAddr;

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";

async fn spawn_app() -> (String, PgPool) {
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
        "TRUNCATE auto_exec_policies, webhook_events_seen, workspace_peer_bindings,
                  audit_events, pipeline_events, approvals, artifacts,
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

async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found")
}

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found")
}

/// Set the default user's bootstrap 'member' role permissions in place.
async fn set_member_permissions(pool: &PgPool, workspace_id: Uuid, perms: Value) {
    sqlx::query(
        "UPDATE roles SET permissions = $2
         WHERE workspace_id = $1 AND name = 'member'",
    )
    .bind(workspace_id)
    .bind(perms)
    .execute(pool)
    .await
    .expect("set member permissions");
}

async fn insert_org(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("insert org")
}

async fn insert_workspace(pool: &PgPool, org_id: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspaces (org_id, name, domain, lifecycle)
         VALUES ($1, $2, 'test', 'continuous'::workspace_lifecycle)
         RETURNING id",
    )
    .bind(org_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert workspace")
}

async fn insert_connector(pool: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO connectors (workspace_id, kind, name, config)
         VALUES ($1, 'rust_native'::connector_kind, $2, '{\"webhook_url\":\"https://example.com/\"}')
         RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert connector")
}

fn policies_url(base: &str, ws: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/auto-exec-policies")
}

fn policy_url(base: &str, ws: Uuid, policy: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/auto-exec-policies/{policy}")
}

fn valid_body(connector_id: Uuid) -> Value {
    json!({
        "name": "auto-file spot weather",
        "trigger": {
            "signal_title_prefix": "Spot weather update",
            "severity_at_most": "flagged"
        },
        "connector_id": connector_id,
        "op": "send",
        "args_template": { "text": "{{signal.title}}: {{signal.body}}" },
        "rate_limit_per_min": 5,
        "severity_cap": "flagged",
        "authorized_by_permission": "approvals:decide"
    })
}

async fn create_policy(base: &str, ws: Uuid, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(policies_url(base, ws))
        .json(body)
        .send()
        .await
        .expect("POST policy")
}

// ─── AC-3a: create round-trip ─────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn policy_create() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let resp = create_policy(&base, ws, &valid_body(connector_id)).await;
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", resp.text().await);
    let created: Value = resp.json().await.expect("create body");
    assert!(created["id"].is_string());
    assert_eq!(created["severity_cap"], "flagged");
    assert_eq!(created["enabled"], json!(true));

    // GET list echoes every submitted field.
    let resp = reqwest::Client::new()
        .get(policies_url(&base, ws))
        .send()
        .await
        .expect("GET policies");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("list body");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    let p = &items[0];
    assert_eq!(p["name"], "auto-file spot weather");
    assert_eq!(p["trigger"]["signal_title_prefix"], "Spot weather update");
    assert_eq!(p["trigger"]["severity_at_most"], "flagged");
    assert_eq!(p["connector_id"], json!(connector_id));
    assert_eq!(p["op"], "send");
    assert_eq!(
        p["args_template"],
        json!({ "text": "{{signal.title}}: {{signal.body}}" })
    );
    assert_eq!(p["rate_limit_per_min"], json!(5));
    assert_eq!(p["severity_cap"], "flagged");
    assert_eq!(p["authorized_by_permission"], "approvals:decide");
    assert_eq!(p["created_by"], json!(default_user_id(&pool).await));
    assert!(p.get("org_id").is_none(), "org_id must not be serialized");
}

// ─── AC-3b: update (full replace) ─────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn policy_update() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let created: Value = create_policy(&base, ws, &valid_body(connector_id))
        .await
        .json()
        .await
        .expect("create body");
    let policy_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();

    let mut body = valid_body(connector_id);
    body["rate_limit_per_min"] = json!(42);
    let resp = reqwest::Client::new()
        .put(policy_url(&base, ws, policy_id))
        .json(&body)
        .send()
        .await
        .expect("PUT policy");
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", resp.text().await);

    let list: Value = reqwest::Client::new()
        .get(policies_url(&base, ws))
        .send()
        .await
        .expect("GET policies")
        .json()
        .await
        .expect("list body");
    assert_eq!(list["items"][0]["rate_limit_per_min"], json!(42));
}

// ─── AC-3c: delete ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn policy_delete() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let created: Value = create_policy(&base, ws, &valid_body(connector_id))
        .await
        .json()
        .await
        .expect("create body");
    let policy_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();

    let resp = reqwest::Client::new()
        .delete(policy_url(&base, ws, policy_id))
        .send()
        .await
        .expect("DELETE policy");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let list: Value = reqwest::Client::new()
        .get(policies_url(&base, ws))
        .send()
        .await
        .expect("GET policies")
        .json()
        .await
        .expect("list body");
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

// ─── AC-4: management gate (403 / cross-org 404) ──────────────────────────────

#[tokio::test]
#[ignore]
async fn policy_gate() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    // Bootstrap member role has no grants — every endpoint must 403.
    let connector_id = insert_connector(&pool, ws, "slack").await;
    let client = reqwest::Client::new();
    let fake_policy = Uuid::new_v4();

    let resp = client.get(policies_url(&base, ws)).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = create_policy(&base, ws, &valid_body(connector_id)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = client
        .put(policy_url(&base, ws, fake_policy))
        .json(&valid_body(connector_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = client
        .delete(policy_url(&base, ws, fake_policy))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Cross-org workspace → 404 (existence not leaked).
    let other_org = insert_org(&pool, "Other Org").await;
    let other_ws = insert_workspace(&pool, other_org, "Other Workspace").await;
    let resp = client
        .get(policies_url(&base, other_ws))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── AC-5: Guard A — authorship escalation ────────────────────────────────────

#[tokio::test]
#[ignore]
async fn authorship_escalation() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    // Actor holds approvals:decide but NOT tool_invoke:slack:*.
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let mut body = valid_body(connector_id);
    body["authorized_by_permission"] = json!("tool_invoke:slack:*");
    let resp = create_policy(&base, ws, &body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let err: Value = resp.json().await.expect("error body");
    assert_eq!(err["error"], "permission_escalation");

    // No row was created.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auto_exec_policies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // admin is exempt.
    set_member_permissions(&pool, ws, json!(["admin"])).await;
    let resp = create_policy(&base, ws, &body).await;
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", resp.text().await);
}

// ─── AC-7: write-time validation fails closed (422, no row) ───────────────────

#[tokio::test]
#[ignore]
async fn validation_fail_closed() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let cases: Vec<(&str, Value)> = vec![
        ("severity_cap command", {
            let mut b = valid_body(connector_id);
            b["severity_cap"] = json!("command");
            b
        }),
        ("severity_cap bananas", {
            let mut b = valid_body(connector_id);
            b["severity_cap"] = json!("bananas");
            b
        }),
        ("trigger severity_at_most command", {
            let mut b = valid_body(connector_id);
            b["trigger"]["severity_at_most"] = json!("command");
            b
        }),
        ("rate_limit_per_min 0", {
            let mut b = valid_body(connector_id);
            b["rate_limit_per_min"] = json!(0);
            b
        }),
        ("rate_limit_per_min 1001", {
            let mut b = valid_body(connector_id);
            b["rate_limit_per_min"] = json!(1001);
            b
        }),
        ("foreign connector (Guard B)", {
            let mut b = valid_body(connector_id);
            b["connector_id"] = json!(Uuid::new_v4());
            b
        }),
    ];

    for (label, body) in cases {
        let resp = create_policy(&base, ws, &body).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "case '{label}' must 422, got {} ({:?})",
            resp.status(),
            resp.text().await
        );
    }

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auto_exec_policies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "no row may be created by a rejected POST");
}

// ─── AC-8: severity_cap defaults to routine ───────────────────────────────────

#[tokio::test]
#[ignore]
async fn severity_cap_default() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;

    let mut body = valid_body(connector_id);
    body.as_object_mut().unwrap().remove("severity_cap");
    let resp = create_policy(&base, ws, &body).await;
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", resp.text().await);
    let created: Value = resp.json().await.expect("create body");
    assert_eq!(
        created["severity_cap"], "routine",
        "omitted severity_cap must default to the safe floor 'routine'"
    );

    let stored: String =
        sqlx::query_scalar("SELECT severity_cap FROM auto_exec_policies WHERE id = $1::uuid")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, "routine");
}

// ─── AC-10: policy audit trail ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn policy_change_audited() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["approvals:decide"])).await;
    let connector_id = insert_connector(&pool, ws, "slack").await;
    let user_id = default_user_id(&pool).await;

    let created: Value = create_policy(&base, ws, &valid_body(connector_id))
        .await
        .json()
        .await
        .expect("create body");
    let policy_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();

    let mut body = valid_body(connector_id);
    body["rate_limit_per_min"] = json!(7);
    let resp = reqwest::Client::new()
        .put(policy_url(&base, ws, policy_id))
        .json(&body)
        .send()
        .await
        .expect("PUT policy");
    assert_eq!(resp.status(), StatusCode::OK);

    let row: (String, String, Value) = sqlx::query_as(
        "SELECT actor_kind::TEXT, actor_ref, payload
         FROM audit_events
         WHERE workspace_id = $1
           AND verb = 'auto_exec_policy.updated'
           AND object_id = $2
         LIMIT 1",
    )
    .bind(ws)
    .bind(policy_id)
    .fetch_one(&pool)
    .await
    .expect("auto_exec_policy.updated audit row missing");

    let (actor_kind, actor_ref, payload) = row;
    assert_eq!(actor_kind, "user");
    assert_eq!(actor_ref, user_id.to_string());
    assert_eq!(payload["actor"], json!(user_id));
    assert_eq!(payload["before"]["rate_limit_per_min"], json!(5));
    assert_eq!(payload["after"]["rate_limit_per_min"], json!(7));

    // created + deleted verbs are also written.
    let create_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE workspace_id = $1 AND verb = 'auto_exec_policy.created'",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(create_count, 1);

    let resp = reqwest::Client::new()
        .delete(policy_url(&base, ws, policy_id))
        .send()
        .await
        .expect("DELETE policy");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let delete_payload: Value = sqlx::query_scalar(
        "SELECT payload FROM audit_events
         WHERE workspace_id = $1 AND verb = 'auto_exec_policy.deleted'
         LIMIT 1",
    )
    .bind(ws)
    .fetch_one(&pool)
    .await
    .expect("auto_exec_policy.deleted audit row missing");
    assert_eq!(delete_payload["before"]["rate_limit_per_min"], json!(7));
}
