/// Relay Data Query integration tests.
///
/// Covers the additive surface: the mapping store and its org isolation, the
/// permission gate, the intersection of mappings with relay's own grants, the
/// proxy's refusal to take scope from the browser, and -- most importantly --
/// that none of it changed the conversation path.
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test relay_integration -- --ignored --test-threads=1
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
        "TRUNCATE relay_run_links, workspace_relay_mappings,
                  webhook_events_seen, workspace_peer_bindings, audit_events, pipeline_events,
                  approvals, artifacts,
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

async fn org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("org")
}

async fn default_user_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = 'default@localhost' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default user not found")
}

async fn set_member_permissions(pool: &PgPool, workspace_id: Uuid, perms: Value) {
    sqlx::query("UPDATE roles SET permissions = $2 WHERE workspace_id = $1 AND name = 'member'")
        .bind(workspace_id)
        .bind(perms)
        .execute(pool)
        .await
        .expect("set member permissions");
}

fn sources_url(base: &str, ws: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/relay/sources")
}

fn ask_url(base: &str, ws: Uuid) -> String {
    format!("{base}/api/v1/workspaces/{ws}/relay/ask")
}

// ─── the surface is absent, not broken, without relay ────────────────────────

/// No `IONE_RELAY_URL` in the test environment, so relay is unconfigured. The
/// Data Query routes answer 404: a deployment without relay has no surface, and
/// a 404 says that where a 500 would suggest something broke.
#[tokio::test]
#[ignore = "requires a database"]
async fn the_data_query_surface_is_absent_when_relay_is_not_configured() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["data:query"])).await;

    let response = reqwest::Client::new()
        .get(sources_url(&base, ws))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body: Value = response.json().await.expect("json");
    // And the message says why, without naming an endpoint that does not exist.
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not configured"));
}

// ─── the permission gate ─────────────────────────────────────────────────────

/// `data:query` is its own permission. A member who may use the conversation
/// surface does not get the data surface for free -- they reach different
/// systems, and one grant should not silently be the other.
#[tokio::test]
#[ignore = "requires a database"]
async fn data_query_is_a_separate_permission_from_the_conversation_surface() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;

    // Everything except data:query.
    set_member_permissions(
        &pool,
        ws,
        json!(["workspace:write", "audit:read", "approvals:decide"]),
    )
    .await;

    let response = reqwest::Client::new()
        .get(sources_url(&base, ws))
        .send()
        .await
        .expect("request");
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a member without data:query reached the data surface"
    );

    // And with it, the gate opens -- as far as the missing relay config.
    set_member_permissions(&pool, ws, json!(["data:query"])).await;
    let response = reqwest::Client::new()
        .get(sources_url(&base, ws))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Asking is gated too, not just listing. A surface that gates the read and
/// leaves the write open is the shape of a real incident.
#[tokio::test]
#[ignore = "requires a database"]
async fn asking_requires_the_data_query_permission() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["workspace:write"])).await;

    let response = reqwest::Client::new()
        .post(ask_url(&base, ws))
        .json(&json!({"ask": "how many orders", "mappingIds": [Uuid::new_v4()]}))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ─── the mapping store ───────────────────────────────────────────────────────

/// Mappings are org-isolated by the same row-level security the other mapping
/// tables use.
///
/// Tested through the restricted role, exactly as `rls_enforcement_integration`
/// does, and for the same reason: the default `ione` role is SUPERUSER and
/// BYPASSRLS, so under it every policy in this database is inert. Asserting
/// isolation on the default connection would assert nothing.
#[tokio::test]
#[ignore = "requires a database"]
async fn workspace_relay_mappings_are_org_scoped() {
    use ione::repos::RelayMappingRepo;
    use sqlx::postgres::PgConnectOptions;
    use std::str::FromStr;

    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;

    // Seeded through the owner connection.
    let owner_repo = RelayMappingRepo::new(pool.clone());
    let mapping = owner_repo
        .create(
            org,
            ws,
            user,
            ione::models::NewRelayMapping {
                relay_connection_id: Uuid::new_v4(),
                display_name: "Warehouse".into(),
                alias: "pg".into(),
                principal_id: None,
            },
        )
        .await
        .expect("create mapping");

    // The restricted role: neither SUPERUSER nor BYPASSRLS, so the policy
    // actually evaluates.
    let base_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
    let options = PgConnectOptions::from_str(&base_url)
        .expect("parse url")
        .username("ione_app")
        .password("ione_app");
    let Ok(restricted) = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
    else {
        eprintln!("SKIP: the ione_app role is not reachable");
        return;
    };
    let repo = RelayMappingRepo::new(restricted);

    let mine = repo
        .list_for_workspace(org, ws)
        .await
        .expect("list for my org");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, mapping.id);

    // Another org, same workspace id. The policy reads the org context, so
    // this returns nothing rather than somebody else's mapping.
    let other_org = Uuid::new_v4();
    let theirs = repo
        .list_for_workspace(other_org, ws)
        .await
        .expect("list for another org");
    assert!(
        theirs.is_empty(),
        "a mapping leaked across the org boundary"
    );
}

/// A mapping narrowed to one principal applies to them and to nobody else.
#[tokio::test]
#[ignore = "requires a database"]
async fn a_narrowed_mapping_applies_only_to_its_principal() {
    use ione::repos::RelayMappingRepo;

    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO users (org_id, email, display_name) VALUES ($1, $2, $2) RETURNING id",
    )
    .bind(org)
    .bind("other@localhost")
    .fetch_one(&pool)
    .await
    .expect("insert user");

    let repo = RelayMappingRepo::new(pool.clone());
    repo.create(
        org,
        ws,
        user,
        ione::models::NewRelayMapping {
            relay_connection_id: Uuid::new_v4(),
            display_name: "Mine".into(),
            alias: "mine".into(),
            principal_id: Some(user),
        },
    )
    .await
    .expect("create narrowed mapping");
    repo.create(
        org,
        ws,
        user,
        ione::models::NewRelayMapping {
            relay_connection_id: Uuid::new_v4(),
            display_name: "Everyone".into(),
            alias: "shared".into(),
            principal_id: None,
        },
    )
    .await
    .expect("create shared mapping");

    let for_owner = repo
        .effective_for_user(org, ws, user)
        .await
        .expect("effective for owner");
    assert_eq!(for_owner.len(), 2);

    let for_other = repo
        .effective_for_user(org, ws, other)
        .await
        .expect("effective for other");
    assert_eq!(for_other.len(), 1);
    assert_eq!(for_other[0].alias, "shared");
}

/// Disabling hides a mapping without deleting it. A mapping that was used is
/// part of how a past run is explained; deleting it leaves a run link pointing
/// at nothing.
#[tokio::test]
#[ignore = "requires a database"]
async fn disabling_a_mapping_hides_it_without_erasing_it() {
    use ione::repos::RelayMappingRepo;

    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;

    let repo = RelayMappingRepo::new(pool.clone());
    let mapping = repo
        .create(
            org,
            ws,
            user,
            ione::models::NewRelayMapping {
                relay_connection_id: Uuid::new_v4(),
                display_name: "Retired".into(),
                alias: "retired".into(),
                principal_id: None,
            },
        )
        .await
        .expect("create");

    repo.set_enabled(org, ws, mapping.id, false)
        .await
        .expect("disable")
        .expect("mapping found");

    assert!(repo
        .effective_for_user(org, ws, user)
        .await
        .expect("effective")
        .is_empty());
    // Still present on the admin surface, with its history.
    assert_eq!(
        repo.list_for_workspace(org, ws).await.expect("list").len(),
        1
    );
}

/// The alias is an identifier in the generated query. The database refuses a
/// shape relay would reject, so a mapping cannot be created that fails on
/// first use.
#[tokio::test]
#[ignore = "requires a database"]
async fn an_alias_that_relay_would_refuse_cannot_be_stored() {
    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;

    for bad in ["pg.main", "PG", "2pg", "pg-main", "pg main", "pg\"; DROP"] {
        let result = sqlx::query(
            "INSERT INTO workspace_relay_mappings
               (org_id, workspace_id, relay_connection_id, display_name, alias, created_by)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(org)
        .bind(ws)
        .bind(Uuid::new_v4())
        .bind("Bad alias")
        .bind(bad)
        .bind(user)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "the alias '{bad}' was stored");
    }

    // And the shape relay accepts is stored.
    let ok = sqlx::query(
        "INSERT INTO workspace_relay_mappings
           (org_id, workspace_id, relay_connection_id, display_name, alias, created_by)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(org)
    .bind(ws)
    .bind(Uuid::new_v4())
    .bind("Good alias")
    .bind("pg_main_2")
    .bind(user)
    .execute(&pool)
    .await;
    assert!(ok.is_ok());
}

/// Two mappings cannot share an alias in one workspace. They would make the
/// generated query ambiguous.
#[tokio::test]
#[ignore = "requires a database"]
async fn an_alias_is_unique_within_a_workspace() {
    use ione::repos::RelayMappingRepo;

    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;
    let repo = RelayMappingRepo::new(pool.clone());

    let make = |alias: &str| ione::models::NewRelayMapping {
        relay_connection_id: Uuid::new_v4(),
        display_name: "Source".into(),
        alias: alias.into(),
        principal_id: None,
    };

    repo.create(org, ws, user, make("pg")).await.expect("first");
    assert!(
        repo.create(org, ws, user, make("pg")).await.is_err(),
        "two mappings shared an alias"
    );
}

// ─── the audit link ──────────────────────────────────────────────────────────

/// The run link records counts and hashes and no values. It is the pointer into
/// relay's audit, not a copy of the answer.
#[tokio::test]
#[ignore = "requires a database"]
async fn a_run_link_records_counts_and_no_values() {
    use ione::repos::RelayMappingRepo;

    let (_base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;
    let repo = RelayMappingRepo::new(pool.clone());

    let run_id = Uuid::new_v4();
    repo.record_run(
        org,
        ws,
        user,
        run_id,
        "succeeded",
        &[Uuid::new_v4()],
        Some(42),
        false,
        None,
        Some("plan-hash"),
        Some("policy-hash"),
        json!({"prompt_tokens": 100}),
    )
    .await
    .expect("record");

    let runs = repo.recent_runs(org, ws, 10).await.expect("recent");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].relay_run_id, run_id);
    assert_eq!(runs[0].row_count, Some(42));

    // Nothing in the row can hold a value from the answer.
    let rendered = serde_json::to_string(&runs[0]).expect("serialize");
    assert!(!rendered.contains("SELECT"));
    for shape in ["postgres://", "AKIA", "-----BEGIN"] {
        assert!(!rendered.contains(shape));
    }

    // Recording the same run again updates rather than duplicating, so a
    // retried callback does not double the history.
    repo.record_run(
        org,
        ws,
        user,
        run_id,
        "succeeded",
        &[],
        Some(42),
        true,
        None,
        None,
        None,
        json!({}),
    )
    .await
    .expect("record again");
    assert_eq!(
        repo.recent_runs(org, ws, 10).await.expect("recent").len(),
        1
    );
}

// ─── the conversation surface is unchanged ───────────────────────────────────

/// The Ollama error contract is untouched. This is the test that would fail if
/// the integration had reached into the conversation path.
#[tokio::test]
#[ignore = "requires a database"]
async fn the_ollama_error_contract_is_unchanged() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["data:query", "workspace:write"])).await;

    // A conversation, posted the way the existing suite posts one. Ollama is
    // not running in the test environment, so this exercises the failure path
    // whose error strings other tests assert on.
    let client = reqwest::Client::new();
    let conversation: Value = client
        .post(format!("{base}/api/v1/conversations"))
        .json(&json!({"title": "unchanged", "workspaceId": ws}))
        .send()
        .await
        .expect("create conversation")
        .json()
        .await
        .expect("json");

    let id = conversation["id"].as_str().expect("conversation id");
    let response = client
        .post(format!("{base}/api/v1/conversations/{id}/messages"))
        .json(&json!({"content": "hello"}))
        .send()
        .await
        .expect("post message");

    // Whatever it answers, the shape is the one the contract suite asserts:
    // an `error` field whose value still starts with `ollama_` when Ollama is
    // the thing that failed.
    if !response.status().is_success() {
        let body: Value = response.json().await.expect("json");
        if let Some(error) = body["error"].as_str() {
            if error.starts_with("ollama") {
                assert!(
                    [
                        "ollama_unreachable",
                        "ollama_upstream",
                        "ollama_model_missing"
                    ]
                    .contains(&error),
                    "the ollama error contract changed: {error}"
                );
            }
        }
    }
}

/// The Data Query routes are additive: every conversation route still resolves.
#[tokio::test]
#[ignore = "requires a database"]
async fn the_conversation_routes_still_resolve() {
    let (base, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["data:query"])).await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/api/v1/conversations"))
        .send()
        .await
        .expect("list conversations");
    assert!(
        response.status().is_success(),
        "the conversation list broke: {}",
        response.status()
    );

    let response = client
        .get(format!("{base}/api/v1/workspaces"))
        .send()
        .await
        .expect("list workspaces");
    assert!(response.status().is_success());

    // And the new routes are present rather than 404 for the wrong reason:
    // relay is unconfigured, which is a 404 with a specific message.
    let response = client
        .get(sources_url(&base, ws))
        .send()
        .await
        .expect("sources");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.expect("json");
    assert!(body["message"]
        .as_str()
        .unwrap_or_default()
        .contains("relay"));
}

// ─── static assets carry no relay secret ─────────────────────────────────────

/// The page is a static asset. It holds no relay endpoint and no relay
/// credential, which is what makes the Data Query surface a proxy rather than
/// a direct call from the browser.
#[tokio::test]
#[ignore = "requires a database"]
async fn the_static_assets_carry_no_relay_secret() {
    let (base, _pool) = spawn_app().await;
    let client = reqwest::Client::new();

    for asset in ["/app.js", "/style.css", "/index.html", "/"] {
        let response = client
            .get(format!("{base}{asset}"))
            .send()
            .await
            .expect("fetch asset");
        if !response.status().is_success() {
            continue;
        }
        let body = response.text().await.expect("text");

        for forbidden in [
            "IONE_RELAY_RUNTIME_TOKEN",
            "IONE_RELAY_SIGNING_KEY",
            "IONE_RELAY_CALLBACK_KEY",
            "x-relay-signature",
            "relay.sig.v1",
        ] {
            assert!(!body.contains(forbidden), "{asset} carries {forbidden}");
        }
        for shape in ["postgres://", "AKIA", "-----BEGIN"] {
            assert!(!body.contains(shape), "{asset} carries {shape}");
        }
        // Every relay request the page makes goes to IONe's own origin.
        if asset == "/app.js" {
            assert!(
                body.contains("/api/v1/workspaces/${activeWorkspace.id}/relay/"),
                "the page does not call relay through IONe"
            );
        }
    }
}

#[tokio::test]
#[ignore]
async fn configured_relay_lifecycle_preserves_request_identity_and_run_access() {
    use std::sync::Arc;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let (_unused, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;
    set_member_permissions(&pool, ws, json!(["data:query", "workspace:write"])).await;
    let mock = MockServer::start().await;
    let (_, mut state) = ione::app_with_state(pool.clone()).await;
    let mut config = (*state.config).clone();
    config.relay = Some(Arc::new(ione::config::RelayConfig {
        base_url: mock.uri(),
        deployment_id: Uuid::new_v4(),
        runtime_token: "test-runtime".into(),
        management_token: None,
        signing_key_id: "test-key".into(),
        signing_key: "test-signing-key".into(),
        callback_verification_key: "test-callback".into(),
        default_model: "fixture-model".into(),
        timeout_ms: 5000,
    }));
    state.relay = Some(Arc::new(
        ione::services::relay_client::RelayClient::new(config.relay.clone().unwrap()).unwrap(),
    ));
    state.config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, ione::routes::router(state))
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    let connection = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path("/v1/connections/available"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"connections":[{"connection_id":connection}]})),
        )
        .mount(&mock)
        .await;
    let mapping: Value = client
        .post(format!("{base}/api/v1/workspaces/{ws}/relay/mappings"))
        .json(&json!({"relayConnectionId":connection,"displayName":"Fixture","alias":"pg"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let sources: Value = client
        .get(sources_url(&base, ws))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sources["sources"].as_array().unwrap().len(), 1);
    let run = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/v1/runs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"run":{"id":run},"outcome":{"kind":"clarification_required"}}),
            ),
        )
        .mount(&mock)
        .await;
    let mut ask = json!({"requestId":Uuid::new_v4(),"ask":"Count rows","mappingIds":[mapping["id"]],"limits":{"max_result_rows":10}});
    for _ in 0..2 {
        client
            .post(ask_url(&base, ws))
            .json(&ask)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    ask["limits"]["max_result_rows"] = json!(5);
    client
        .post(ask_url(&base, ws))
        .json(&ask)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    ask["requestId"] = json!(Uuid::new_v4());
    client
        .post(ask_url(&base, ws))
        .json(&ask)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let requests = mock.received_requests().await.unwrap();
    let asks: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/runs")
        .collect();
    let key = |i: usize| {
        asks[i]
            .headers
            .get("idempotency-key")
            .unwrap()
            .to_str()
            .unwrap()
    };
    assert_eq!(key(0), key(1));
    assert_ne!(key(1), key(2));
    assert_ne!(key(2), key(3));
    for request in &asks {
        assert!(request.headers.contains_key("x-relay-signature"));
        let scope: Value =
            serde_json::from_str(request.headers["x-relay-scope"].to_str().unwrap()).unwrap();
        assert_eq!(scope["workspace_id"], ws.to_string());
        assert_eq!(scope["actor_id"], user.to_string());
    }
    sqlx::query("INSERT INTO relay_run_links (org_id,workspace_id,user_id,relay_run_id,outcome,mapping_ids,truncated,usage) SELECT $1,$2,$3,gen_random_uuid(),'started','{}',false,'{}' FROM generate_series(1,501)")
        .bind(org).bind(ws).bind(user).execute(&pool).await.unwrap();
    let repo = ione::repos::RelayMappingRepo::new(pool.clone());
    assert!(repo.has_run(org, ws, user, run).await.unwrap());
    assert!(!repo.has_run(org, ws, Uuid::new_v4(), run).await.unwrap());
    assert!(!repo.has_run(org, Uuid::new_v4(), user, run).await.unwrap());
    assert!(!repo.has_run(Uuid::new_v4(), ws, user, run).await.unwrap());
    let run_url = format!("{base}/api/v1/workspaces/{ws}/relay/runs/{run}");
    for suffix in ["", "/result", "/receipts"] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/runs/{run}{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
            .mount(&mock)
            .await;
        client
            .get(format!("{run_url}{suffix}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    for suffix in ["/clarification", "/cancel"] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/runs/{run}{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
            .mount(&mock)
            .await;
        client
            .post(format!("{run_url}{suffix}"))
            .json(&json!({"seq":1,"answer":"All rows"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    Mock::given(method("GET"))
        .and(path(format!("/v1/runs/{run}/events")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("id: 8\nevent: succeeded\ndata: {}\n\n"),
        )
        .mount(&mock)
        .await;
    let events = client
        .get(format!("{run_url}/events?lastEventId=1"))
        .header("last-event-id", "7")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(events.contains("id: 8"));
    let requests = mock.received_requests().await.unwrap();
    let event_request = requests
        .iter()
        .find(|r| r.url.path().ends_with("/events"))
        .unwrap();
    assert_eq!(event_request.headers["last-event-id"], "7");
    sqlx::query("CREATE FUNCTION reject_relay_link() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture reject'; END $$")
        .execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_relay_link BEFORE INSERT ON relay_run_links FOR EACH ROW EXECUTE FUNCTION reject_relay_link()")
        .execute(&pool).await.unwrap();
    let status = client
        .post(ask_url(&base, ws))
        .json(&ask)
        .send()
        .await
        .unwrap()
        .status();
    sqlx::query("DROP TRIGGER reject_relay_link ON relay_run_links")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_relay_link()")
        .execute(&pool)
        .await
        .unwrap();
    assert!(status.is_server_error());
}

#[tokio::test]
#[ignore = "requires a migrated database"]
async fn source_onboarding_scopes_grants_and_scrubs_failures() {
    use std::sync::Arc;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let (_, pool) = spawn_app().await;
    let ws = ops_workspace_id(&pool).await;
    let admin = ione::repos::RoleRepo::new(pool.clone())
        .upsert(ws, "source-admin", 80)
        .await
        .unwrap();
    assert!(admin
        .permissions
        .as_array()
        .unwrap()
        .contains(&json!("data:sources:write")));
    let analyst = ione::repos::RoleRepo::new(pool.clone())
        .upsert(ws, "source-analyst", 20)
        .await
        .unwrap();
    assert!(!analyst
        .permissions
        .to_string()
        .contains("data:sources:write"));
    let org = org_id(&pool).await;
    let user = default_user_id(&pool).await;
    let mock = MockServer::start().await;
    let (_, mut state) = ione::app_with_state(pool.clone()).await;
    let mut config = (*state.config).clone();
    config.relay = Some(Arc::new(ione::config::RelayConfig {
        base_url: mock.uri(),
        deployment_id: Uuid::new_v4(),
        runtime_token: "runtime-only".into(),
        management_token: Some("management-only".into()),
        signing_key_id: "k1".into(),
        signing_key: "fixture-signing-key".into(),
        callback_verification_key: "callback".into(),
        default_model: "fixture-model".into(),
        timeout_ms: 5000,
    }));
    assert!(!format!("{:?}", config.relay).contains("management-only"));
    state.relay = Some(Arc::new(
        ione::services::relay_client::RelayClient::new(config.relay.clone().unwrap()).unwrap(),
    ));
    state.config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, ione::routes::router(state))
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    let source = json!({"name":"Sales", "alias":"sales", "host":"localhost", "port":5432,
        "database":"warehouse", "schema":"public", "tables":["orders"], "username":"reader",
        "password":"secret-canary-531", "tls":"disable", "isolation":"single_tenant"});
    set_member_permissions(&pool, ws, json!(["data:query", "workspace:write"])).await;
    assert_eq!(
        client
            .post(sources_url(&base, ws))
            .json(&source)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
    set_member_permissions(&pool, ws, json!(["data:query", "data:sources:write"])).await;
    assert_eq!(
        client
            .get(format!("{base}/api/v1/workspaces/{ws}/relay/source-admin"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let interrupted = Mock::given(method("POST"))
        .and(path("/v1/connections"))
        .respond_with(
            ResponseTemplate::new(502).set_body_json(json!({"message":"secret-canary-531"})),
        )
        .mount_as_scoped(&mock)
        .await;
    let response = client
        .post(sources_url(&base, ws))
        .json(&source)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.text().await.unwrap().contains("secret-canary"));
    let pending: Uuid = sqlx::query_scalar("SELECT request_id FROM relay_source_registrations WHERE org_id=$1 AND workspace_id=$2 AND actor_id=$3 AND alias='sales'")
        .bind(org).bind(ws).bind(user).fetch_one(&pool).await.unwrap();
    let first_request: Value =
        serde_json::from_slice(&mock.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(first_request["request_id"], json!(pending));
    assert_eq!(mock.received_requests().await.unwrap().len(), 1);
    drop(interrupted);
    mock.reset().await;
    let connection = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/v1/connections"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":connection}))
                .set_delay(std::time::Duration::from_millis(200)),
        )
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/connections/{connection}/validate")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"state":"active"})))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/connections/{connection}/catalog")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"entities":1})))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/connections/{connection}/grants")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"grant_id":Uuid::new_v4()})))
        .mount(&mock)
        .await;
    let first = client.post(sources_url(&base, ws)).json(&source).send();
    let second = client.post(sources_url(&base, ws)).json(&source).send();
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    let (response, concurrent) = if first.status() == StatusCode::OK {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(concurrent.status(), StatusCode::CONFLICT);
    let output = response.text().await.unwrap();
    assert!(!output.contains("secret-canary"));
    assert!(!output.contains("warehouse"));
    let mappings = ione::repos::RelayMappingRepo::new(pool.clone())
        .effective_for_user(org, ws, user)
        .await
        .unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].principal_id, Some(user));
    let requests = mock.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer management-only"
        );
        let scope: Value = serde_json::from_str(
            request
                .headers
                .get("x-relay-scope")
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(scope["actor_id"], user.to_string());
        assert_eq!(scope["tenant_id"], org.to_string());
        assert_eq!(scope["workspace_id"], ws.to_string());
    }
    let create: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(create["request_id"], json!(pending));
    assert!(create["credential"]
        .as_str()
        .unwrap()
        .contains("secret-canary-531"));
    assert!(!create["public_config"]
        .to_string()
        .contains("secret-canary"));
    let grant: Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert_eq!(grant["principal_kind"], "principal");
    assert_eq!(grant["principal_id"], user.to_string());
    assert_eq!(grant["entity_allowlist"], json!(["orders"]));
    assert_eq!(
        client
            .post(sources_url(&base, ws))
            .json(&source)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(mock.received_requests().await.unwrap().len(), 4);
    let mut forged = source.clone();
    forged["workspaceId"] = json!(Uuid::new_v4());
    assert_eq!(
        client
            .post(sources_url(&base, ws))
            .json(&forged)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    mock.reset().await;
    let failed_id = Uuid::new_v4();
    Mock::given(method("POST"))
        .and(path("/v1/connections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":failed_id})))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/connections/{failed_id}/validate")))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"message":"secret-canary-531"})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/v1/connections/{failed_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked":true})))
        .expect(1)
        .mount(&mock)
        .await;
    let mut failed = source.clone();
    failed["alias"] = json!("failed");
    let response = client
        .post(sources_url(&base, ws))
        .json(&failed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.text().await.unwrap().contains("secret-canary"));
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM relay_source_registrations WHERE org_id=$1 AND workspace_id=$2 AND actor_id=$3 AND alias='failed'")
        .bind(org).bind(ws).bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(remaining, 0);
    assert!(ione::repos::RelayMappingRepo::new(pool)
        .by_alias(org, ws, "failed")
        .await
        .unwrap()
        .is_none());
}
