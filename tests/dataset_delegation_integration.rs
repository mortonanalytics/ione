use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
async fn permissions(pool: &PgPool, ws: Uuid, enabled: bool) {
    sqlx::query("UPDATE roles SET permissions=$2 WHERE workspace_id=$1 AND name='member'")
        .bind(ws)
        .bind(if enabled {
            json!(["data:query", "data:datasets:write"])
        } else {
            json!(["admin", "data:query"])
        })
        .execute(pool)
        .await
        .unwrap();
}

async fn metadata_rpc(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    origin: &Value,
    input: Value,
) -> reqwest::Response {
    client
        .post(url)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .json(&input)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated migrated PostgreSQL"]
async fn immutable_delegation_checks_every_identity_permission_expiry_and_download() {
    let url = std::env::var("DATABASE_URL").expect("isolated DATABASE_URL required");
    assert!(
        url.ends_with("/ione_dataset_delegations"),
        "never truncate live database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query("TRUNCATE organizations CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    let (_, mut state) = ione::app_with_state(pool.clone()).await;
    let ws: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE name='Operations'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let actor: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='default@localhost'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let org: Uuid = sqlx::query_scalar("SELECT org_id FROM workspaces WHERE id=$1")
        .bind(ws)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO org_memberships(user_id,org_id,permissions) VALUES($1,$2,'[]') ON CONFLICT DO NOTHING").bind(actor).bind(org).execute(&pool).await.unwrap();
    permissions(&pool, ws, true).await;
    let mock = MockServer::start().await;
    let deployment = Uuid::new_v4();
    let mut config = (*state.config).clone();
    config.relay = Some(Arc::new(ione::config::RelayConfig {
        base_url: mock.uri(),
        deployment_id: deployment,
        runtime_token: "runtime-secret-canary".into(),
        management_token: Some("management-secret-canary".into()),
        signing_key_id: "fixture".into(),
        signing_key: "fixture-key".into(),
        callback_verification_key: "fixture-callback".into(),
        default_model: "fixture".into(),
        timeout_ms: 5000,
    }));
    state.relay = Some(Arc::new(
        ione::services::relay_client::RelayClient::new(config.relay.clone().unwrap()).unwrap(),
    ));
    state.config = Arc::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, ione::routes::router(state))
            .await
            .unwrap()
    });
    let dataset = Uuid::new_v4();
    let version = Uuid::new_v4();
    let body = b"bounded binary fixture".to_vec();
    let manifest = Arc::new(Mutex::new(
        json!({"deployment_id":deployment,"tenant_id":org,"workspace_id":ws,"dataset_id":dataset,"version_id":version,"requires_source_access":false,"allows_delegation":true,"lineage":[{"connection_id":Uuid::new_v4()}],"schema":[{"name":"amount","type":"Int64","nullable":false}],"schema_ipc_base64":STANDARD.encode(b"schema fixture"),"arrow_schema_hash":digest(b"schema fixture"),"digest":digest(&body),"row_count":200,"classification":"internal","expires_at":Utc::now()+Duration::minutes(30)}),
    ));
    let value = manifest.clone();
    Mock::given(method("GET"))
        .and(path(format!("/v1/datasets/{dataset}/versions/{version}")))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(value.lock().unwrap().clone())
        })
        .mount(&mock)
        .await;
    let projected = b"projected binary fixture".to_vec();
    let read_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = read_count.clone();
    let template = ResponseTemplate::new(200)
        .insert_header("x-relay-source-digest", digest(&body))
        .insert_header("x-relay-content-digest", digest(&projected))
        .insert_header("x-relay-row-count", "200")
        .set_body_bytes(projected.clone())
        .set_delay(std::time::Duration::from_millis(150));
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/datasets/{dataset}/versions/{version}/read"
        )))
        .respond_with(move |_: &wiremock::Request| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            template.clone()
        })
        .mount(&mock)
        .await;
    let client = reqwest::Client::new();
    let root = format!(
        "{base}/api/v1/workspaces/{ws}/relay/datasets/{dataset}/versions/{version}/delegations"
    );
    let origin = json!({"deployment_id":Uuid::new_v4(),"tenant_id":Uuid::new_v4(),"workspace_id":Uuid::new_v4(),"actor_id":"origin-actor","service_account_id":"origin-account"});
    let input = json!({"origin":origin,"ttlSeconds":3600});
    let mut unknown = input.clone();
    unknown["owner"] = json!({"actor_id":"spoof"});
    assert_eq!(
        client
            .post(&root)
            .json(&unknown)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let mut unknown_origin = input.clone();
    unknown_origin["origin"]["credential"] = json!("secret-canary");
    assert_eq!(
        client
            .post(&root)
            .json(&unknown_origin)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let mut cycle = input.clone();
    cycle["origin"]["deployment_id"] = json!(deployment);
    assert_eq!(
        client
            .post(&root)
            .json(&cycle)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut excessive = input.clone();
    excessive["ttlSeconds"] = json!(3601);
    assert_eq!(
        client
            .post(&root)
            .json(&excessive)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let response = client.post(&root).json(&input).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let created: Value = response.json().await.unwrap();
    let grant = created["grant"]["grant_id"].as_str().unwrap();
    let token = created["token"].as_str().unwrap();
    assert_eq!(token.len(), 51);
    assert_eq!(created["grant"]["owner"]["actor_id"], actor.to_string());
    let stored: Value = sqlx::query_scalar("SELECT manifest FROM dataset_delegations WHERE id=$1")
        .bind(Uuid::parse_str(grant).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!stored.to_string().contains(token));
    let listed = client
        .get(&root)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!listed.contains(token));
    assert!(!listed.contains("token_hash"));
    let desc = format!("{base}/api/v1/dataset-delegations/{grant}/descriptor");
    let read = format!("{base}/api/v1/dataset-delegations/{grant}/read");
    let descriptor = client
        .get(&desc)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(descriptor.status(), 200);
    let descriptor: Value = descriptor.json().await.unwrap();
    assert_eq!(descriptor["row_count"], 200);
    assert_eq!(descriptor["read_only"], true);
    assert!(descriptor.get("rows").is_none());
    let mcp = format!("{base}/api/v1/dataset-delegations/{grant}/mcp");
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}});
    let initialized = metadata_rpc(&client, &mcp, token, &origin, initialize.clone()).await;
    assert_eq!(initialized.status(), 200);
    assert_eq!(initialized.headers()["cache-control"], "no-store");
    assert_eq!(initialized.headers()["mcp-protocol-version"], "2025-11-25");
    assert!(initialized.headers().get("mcp-session-id").is_none());
    assert_eq!(
        initialized.json::<Value>().await.unwrap()["result"]["protocolVersion"],
        "2025-11-25"
    );
    let notification = metadata_rpc(
        &client,
        &mcp,
        token,
        &origin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(notification.status(), 202);
    assert_eq!(notification.headers()["cache-control"], "no-store");
    assert!(notification.bytes().await.unwrap().is_empty());
    let tools = metadata_rpc(
        &client,
        &mcp,
        token,
        &origin,
        json!({"jsonrpc":"2.0","id":"tools","method":"tools/list"}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(tools["result"]["tools"][0]["name"], "describe_dataset");
    assert_eq!(
        tools["result"]["tools"][0]["annotations"]["readOnlyHint"],
        true
    );
    let described=metadata_rpc(&client,&mcp,token,&origin,json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"describe_dataset","arguments":{}}})).await.json::<Value>().await.unwrap();
    assert_eq!(
        described["result"]["structuredContent"]["descriptor"],
        descriptor
    );
    assert_eq!(
        described["result"]["structuredContent"]["read"]["path"],
        format!("/api/v1/dataset-delegations/{grant}/read")
    );
    let resources = metadata_rpc(
        &client,
        &mcp,
        token,
        &origin,
        json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    let uri = resources["result"]["resources"][0]["uri"].as_str().unwrap();
    assert_eq!(
        uri,
        format!("ione-dataset://{grant}/{dataset}/versions/{version}")
    );
    let resource = metadata_rpc(
        &client,
        &mcp,
        token,
        &origin,
        json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":uri}}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    let resource_metadata: Value =
        serde_json::from_str(resource["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(resource_metadata, described["result"]["structuredContent"]);
    for (request, code) in [
        (json!([initialize.clone()]), -32600),
        (json!({"jsonrpc":"2.0","id":5,"method":"unknown"}), -32601),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"publish_dataset","arguments":{}}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"describe_dataset","arguments":{"sql":"SELECT secret"}}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"file:///etc/passwd"}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":format!("ione-dataset://{}/{dataset}/versions/{version}",Uuid::new_v4())}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":format!("ione-dataset://{grant}/{dataset}/versions/{}",Uuid::new_v4())}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"cursor":"other"}}),
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"tools/list","extra":"no"}),
            -32600,
        ),
    ] {
        assert_eq!(
            metadata_rpc(&client, &mcp, token, &origin, request)
                .await
                .json::<Value>()
                .await
                .unwrap()["error"]["code"],
            code
        );
    }
    for credential in ["", "ione_sat_fake", "generic.ione.jwt"] {
        let response = metadata_rpc(&client, &mcp, credential, &origin, initialize.clone()).await;
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let oversized = client
        .post(&mcp)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .header("content-type", "application/json")
        .body(" ".repeat(65537))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    assert_eq!(oversized.headers()["cache-control"], "no-store");
    for secret in [
        token,
        "runtime-secret-canary",
        "management-secret-canary",
        "bounded binary fixture",
    ] {
        assert!(!described.to_string().contains(secret));
        assert!(!resource.to_string().contains(secret));
    }
    assert!(mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.method == "GET"));
    let previous_schema = manifest.lock().unwrap()["schema_ipc_base64"].clone();
    let previous_hash = manifest.lock().unwrap()["arrow_schema_hash"].clone();
    let large_schema = vec![1u8; 30000];
    manifest.lock().unwrap()["schema_ipc_base64"] = json!(STANDARD.encode(&large_schema));
    manifest.lock().unwrap()["arrow_schema_hash"] = json!(digest(&large_schema));
    let large_grant: Value = client
        .post(&root)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let limited = metadata_rpc(
        &client,
        &format!(
            "{base}/api/v1/dataset-delegations/{}/mcp",
            large_grant["grant"]["grant_id"].as_str().unwrap()
        ),
        large_grant["token"].as_str().unwrap(),
        &origin,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"describe_dataset"}}),
    )
    .await
    .bytes()
    .await
    .unwrap();
    assert!(limited.len() <= 65536);
    assert_eq!(
        serde_json::from_slice::<Value>(&limited).unwrap()["error"]["code"],
        -32603
    );
    manifest.lock().unwrap()["schema_ipc_base64"] = previous_schema;
    manifest.lock().unwrap()["arrow_schema_hash"] = previous_hash;
    for field in [
        "deployment_id",
        "tenant_id",
        "workspace_id",
        "actor_id",
        "service_account_id",
    ] {
        let mut wrong = origin.clone();
        wrong[field] = json!(if field.ends_with("_id")
            && !["actor_id", "service_account_id"].contains(&field)
        {
            Uuid::new_v4().to_string()
        } else {
            "different".into()
        });
        assert_eq!(
            client
                .get(&desc)
                .bearer_auth(token)
                .header("x-ione-dataset-origin", wrong.to_string())
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{field}"
        );
        assert_eq!(
            metadata_rpc(&client, &mcp, token, &wrong, initialize.clone())
                .await
                .status(),
            401
        );
    }
    for credential in ["", "ione_sat_bad", "peer-jwt", "runtime-secret-canary"] {
        assert_eq!(
            client
                .get(&desc)
                .bearer_auth(credential)
                .header("x-ione-dataset-origin", origin.to_string())
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let request = json!({"columns":["amount"],"limit":null,"max_rows":200,"max_bytes":1024});
    let response = client
        .post(&read)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-relay-row-count"], "200");
    assert_eq!(response.bytes().await.unwrap(), projected);
    let mut invalid = request.clone();
    invalid["columns"] = json!(["secret"]);
    assert_eq!(
        client
            .post(&read)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut too_small = request.clone();
    too_small["max_bytes"] = json!(2);
    assert_eq!(
        client
            .post(&read)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .json(&too_small)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let mut few_rows = request.clone();
    few_rows["max_rows"] = json!(1);
    assert_eq!(
        client
            .post(&read)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .json(&few_rows)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    for (field, new_value) in [
        ("deployment_id", json!(Uuid::new_v4())),
        ("tenant_id", json!(Uuid::new_v4())),
        ("workspace_id", json!(Uuid::new_v4())),
        ("actor_id", json!(Uuid::new_v4().to_string())),
        ("service_account_id", json!("wrong-owner-account")),
    ] {
        sqlx::query(
            "UPDATE dataset_delegations SET manifest=jsonb_set(manifest,$2,$3) WHERE id=$1",
        )
        .bind(Uuid::parse_str(grant).unwrap())
        .bind(vec!["owner", field])
        .bind(new_value)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !client
                .get(&desc)
                .bearer_auth(token)
                .header("x-ione-dataset-origin", origin.to_string())
                .send()
                .await
                .unwrap()
                .status()
                .is_success(),
            "owner {field}"
        );
        sqlx::query("UPDATE dataset_delegations SET manifest=$2 WHERE id=$1")
            .bind(Uuid::parse_str(grant).unwrap())
            .bind(stored.clone())
            .execute(&pool)
            .await
            .unwrap();
    }
    permissions(&pool, ws, false).await;
    assert_eq!(
        metadata_rpc(&client, &mcp, token, &origin, initialize.clone())
            .await
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&desc)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&root)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    permissions(&pool, ws, true).await;
    sqlx::query("DELETE FROM org_memberships WHERE user_id=$1 AND org_id=$2")
        .bind(actor)
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&desc)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("INSERT INTO org_memberships(user_id,org_id,permissions) VALUES($1,$2,'[]')")
        .bind(actor)
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    for (field, value) in [
        ("requires_source_access", json!(true)),
        ("allows_delegation", json!(false)),
        ("allows_delegation", Value::Null),
        ("lineage", json!([{"remote":{"grant_id":Uuid::new_v4()}}])),
        ("arrow_schema_hash", json!("sha256:wrong")),
        ("digest", json!(digest(b"mutated"))),
    ] {
        let previous = manifest.lock().unwrap()[field].clone();
        manifest.lock().unwrap()[field] = value;
        assert_eq!(
            client
                .get(&desc)
                .bearer_auth(token)
                .header("x-ione-dataset-origin", origin.to_string())
                .send()
                .await
                .unwrap()
                .status(),
            if field == "requires_source_access" {
                404
            } else {
                403
            },
            "{field}"
        );
        if field != "digest" {
            assert_eq!(
                client
                    .post(&root)
                    .json(&input)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                if field == "requires_source_access" {
                    404
                } else {
                    403
                },
                "{field}"
            );
        }
        assert_eq!(
            metadata_rpc(&client, &mcp, token, &origin, initialize.clone())
                .await
                .status(),
            if field == "requires_source_access" {
                404
            } else {
                403
            }
        );
        manifest.lock().unwrap()[field] = previous;
    }
    let before = read_count.load(std::sync::atomic::Ordering::SeqCst);
    let in_flight = client
        .post(&read)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .json(&request);
    let downloading = tokio::spawn(async move { in_flight.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while read_count.load(std::sync::atomic::Ordering::SeqCst) == before {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("projection must be in flight before revocation");
    permissions(&pool, ws, false).await;
    assert_eq!(downloading.await.unwrap().status(), 403);
    permissions(&pool, ws, true).await;
    let before = read_count.load(std::sync::atomic::Ordering::SeqCst);
    let in_flight = client
        .post(&read)
        .bearer_auth(token)
        .header("x-ione-dataset-origin", origin.to_string())
        .json(&request);
    let downloading = tokio::spawn(async move { in_flight.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while read_count.load(std::sync::atomic::Ordering::SeqCst) == before {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("projection must be in flight before revocation");
    assert_eq!(
        client
            .delete(format!("{root}/{grant}"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(downloading.await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(&desc)
            .bearer_auth(token)
            .header("x-ione-dataset-origin", origin.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        metadata_rpc(
            &client,
            &mcp,
            token,
            &origin,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .status(),
        401
    );
    let second: Value = client
        .post(&root)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_id = second["grant"]["grant_id"].as_str().unwrap();
    sqlx::query("UPDATE dataset_delegations SET expires_at=now()-interval '1 second',created_at=now()-interval '2 seconds' WHERE id=$1").bind(Uuid::parse_str(second_id).unwrap()).execute(&pool).await.unwrap();
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/dataset-delegations/{second_id}/descriptor"
            ))
            .bearer_auth(second["token"].as_str().unwrap())
            .header("x-ione-dataset-origin", origin.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        metadata_rpc(
            &client,
            &format!("{base}/api/v1/dataset-delegations/{second_id}/mcp"),
            second["token"].as_str().unwrap(),
            &origin,
            initialize
        )
        .await
        .status(),
        401
    );
    for req in mock.received_requests().await.unwrap() {
        assert!(!format!("{:?}", req.headers).contains(token));
        assert!(!String::from_utf8_lossy(&req.body).contains(token));
    }
    for secret in [token, "runtime-secret-canary", "management-secret-canary"] {
        assert!(!descriptor.to_string().contains(secret));
        assert!(!listed.contains(secret));
    }
}
