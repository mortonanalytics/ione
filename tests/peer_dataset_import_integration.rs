use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
#[ignore = "requires isolated migrated PostgreSQL"]
async fn peer_import_pins_identity_retries_and_revokes_current_mapping_access() {
    let url = std::env::var("DATABASE_URL").unwrap();
    assert!(url.ends_with("/ione_dataset_delegations"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
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
    let org: Uuid = sqlx::query_scalar("SELECT org_id FROM workspaces WHERE id=$1")
        .bind(ws)
        .fetch_one(&pool)
        .await
        .unwrap();
    let actor: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='default@localhost'")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO org_memberships(user_id,org_id,permissions) VALUES($1,$2,'[]') ON CONFLICT DO NOTHING").bind(actor).bind(org).execute(&pool).await.unwrap();
    sqlx::query("UPDATE roles SET permissions=$2 WHERE workspace_id=$1 AND name='member'")
        .bind(ws)
        .bind(json!([
            "data:query",
            "data:sources:write",
            "workspace:write"
        ]))
        .execute(&pool)
        .await
        .unwrap();
    state.pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&url.replace("ione:ione@", "ione_app:ione_app@"))
        .await
        .expect("restricted app role must exercise RLS");
    let owner = MockServer::start().await;
    let relay = MockServer::start().await;
    let deployment = Uuid::new_v4();
    let owner_deployment = Uuid::new_v4();
    let owner_org = Uuid::new_v4();
    let owner_ws = Uuid::new_v4();
    let grant = Uuid::new_v4();
    let connection = Uuid::new_v4();
    let mut config = (*state.config).clone();
    config.allow_private_peers = true;
    config.private_peer_allowlist = vec!["127.0.0.1".into()];
    config.relay = Some(Arc::new(ione::config::RelayConfig {
        base_url: relay.uri(),
        deployment_id: deployment,
        runtime_token: "runtime-canary".into(),
        management_token: Some("management-canary".into()),
        signing_key_id: "fixture".into(),
        signing_key: "fixture-key".into(),
        callback_verification_key: "callback".into(),
        default_model: "fixture".into(),
        timeout_ms: 1000,
    }));
    state.relay = Some(Arc::new(
        ione::services::relay_client::RelayClient::new(config.relay.clone().unwrap()).unwrap(),
    ));
    state.config = Arc::new(config);
    let issuer:Uuid=sqlx::query_scalar("INSERT INTO trust_issuers(org_id,issuer_url,audience,jwks_uri,claim_mapping) VALUES($1,$2,'fixture',$2,'{}') RETURNING id").bind(org).bind(owner.uri()).fetch_one(&pool).await.unwrap();
    let peer:Uuid=sqlx::query_scalar("INSERT INTO peers(org_id,name,mcp_url,issuer_id,sharing_policy) VALUES($1,'Owner',$2,$3,$4) RETURNING id").bind(org).bind(format!("{}/mcp",owner.uri())).bind(issuer).bind(json!({"deployment_id":owner_deployment})).fetch_one(&pool).await.unwrap();
    let binding:Uuid=sqlx::query_scalar("INSERT INTO workspace_peer_bindings(org_id,workspace_id,peer_id,foreign_tenant_id,foreign_workspace_id,status) VALUES($1,$2,$3,$4,$5,'active') RETURNING id").bind(org).bind(ws).bind(peer).bind(owner_org.to_string()).bind(owner_ws.to_string()).fetch_one(&pool).await.unwrap();
    let origin = json!({"deployment_id":deployment,"tenant_id":org,"workspace_id":ws,"actor_id":actor.to_string(),"service_account_id":format!("ione:{org}")});
    let descriptor = Arc::new(Mutex::new(
        json!({"grant_id":grant,"owner":{"deployment_id":owner_deployment,"tenant_id":owner_org,"workspace_id":owner_ws,"actor_id":"owner","service_account_id":"owner-account"},"origin":origin,"dataset_id":Uuid::new_v4(),"version_id":Uuid::new_v4(),"schema_ipc_base64":STANDARD.encode(b"schema fixture"),"arrow_schema_hash":format!("sha256:{}",hex::encode(Sha256::digest(b"schema fixture"))),"content_digest":format!("sha256:{}","a".repeat(64)),"row_count":200,"classification":"internal","expires_at":Utc::now()+Duration::minutes(20),"columns":["amount"],"read_only":true}),
    ));
    let response = descriptor.clone();
    let redirect = Arc::new(AtomicBool::new(false));
    let redirecting = redirect.clone();
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/dataset-delegations/{grant}/descriptor"
        )))
        .respond_with(move |_: &wiremock::Request| {
            if redirecting.load(Ordering::SeqCst) {
                ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/")
            } else {
                ResponseTemplate::new(200).set_body_json(response.lock().unwrap().clone())
            }
        })
        .mount(&owner)
        .await;
    let nonce = Arc::new(Mutex::new(None::<String>));
    let seen = nonce.clone();
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    Mock::given(method("POST"))
        .and(path("/v1/connections"))
        .respond_with(move |r: &wiremock::Request| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(b["kind"], "ione_dataset");
            let id = b["request_id"].as_str().unwrap().to_owned();
            let mut old = seen.lock().unwrap();
            if let Some(ref previous) = *old {
                assert_eq!(previous, &id);
            }
            *old = Some(id);
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"id":connection}))
            }
        })
        .mount(&relay)
        .await;
    for (suffix, body) in [
        ("validate", json!({"state":"active"})),
        ("catalog", json!({"entities":1})),
        ("grants", json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/connections/{connection}/{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&relay)
            .await;
    }
    Mock::given(method("GET")).and(path("/v1/connections/available")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"connections":[{"connection_id":connection,"kind":"ione_dataset","entities":[]}]}))).mount(&relay).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, ione::routes::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let root = format!("{base}/api/v1/workspaces/{ws}/relay");
    assert_eq!(
        client
            .get(format!("{root}/dataset-origin"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        origin
    );
    assert_eq!(
        client
            .get(format!("{root}/peer-datasets"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["peers"][0]["id"],
        peer.to_string()
    );
    let token = format!("ione_dg_{}", "a".repeat(43));
    let input = json!({"peerId":peer,"grantId":grant,"token":token});
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/descriptor"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for field in [
        "deployment_id",
        "tenant_id",
        "workspace_id",
        "actor_id",
        "service_account_id",
    ] {
        let prior = descriptor.lock().unwrap()["origin"][field].clone();
        descriptor.lock().unwrap()["origin"][field] = json!(Uuid::new_v4().to_string());
        assert_eq!(
            client
                .post(format!("{root}/peer-datasets/descriptor"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            403,
            "{field}"
        );
        descriptor.lock().unwrap()["origin"][field] = prior;
    }
    for (field, bad) in [
        ("arrow_schema_hash", json!("bad")),
        ("expires_at", json!(Utc::now() - Duration::seconds(1))),
        ("read_only", json!(false)),
    ] {
        let prior = descriptor.lock().unwrap()[field].clone();
        descriptor.lock().unwrap()[field] = bad;
        assert_eq!(
            client
                .post(format!("{root}/peer-datasets/descriptor"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        descriptor.lock().unwrap()[field] = prior;
    }
    sqlx::query("UPDATE peers SET sharing_policy=$2 WHERE id=$1")
        .bind(peer)
        .bind(json!({"deployment_id":Uuid::new_v4()}))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/descriptor"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("UPDATE peers SET sharing_policy=$2 WHERE id=$1")
        .bind(peer)
        .bind(json!({"deployment_id":owner_deployment}))
        .execute(&pool)
        .await
        .unwrap();
    let columns = descriptor.lock().unwrap()["columns"].clone();
    descriptor.lock().unwrap()["columns"] = json!(["x".repeat(65536)]);
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/descriptor"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    descriptor.lock().unwrap()["columns"] = columns;
    redirect.store(true, Ordering::SeqCst);
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/descriptor"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    redirect.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE peers SET mcp_url='http://169.254.169.254/mcp' WHERE id=$1")
        .bind(peer)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/descriptor"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("UPDATE peers SET mcp_url=$2 WHERE id=$1")
        .bind(peer)
        .bind(format!("{}/mcp", owner.uri()))
        .execute(&pool)
        .await
        .unwrap();
    let mut import = input.clone();
    import["alias"] = json!("peer_report");
    import["name"] = json!("Peer report");
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/import"))
            .json(&import)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let response = client
        .post(format!("{root}/peer-datasets/import"))
        .json(&import)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "{}",
        response.text().await.unwrap_or_default()
    );
    let mapping = ione::repos::RelayMappingRepo::new(pool.clone())
        .by_alias(org, ws, "peer_report")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.principal_id, Some(actor));
    assert_eq!(creates.load(Ordering::SeqCst), 2);
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/import"))
            .json(&import)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(creates.load(Ordering::SeqCst), 2);
    assert_eq!(
        client
            .post(format!("{root}/mappings"))
            .json(&json!({"relayConnectionId":connection,"displayName":"Bypass","alias":"bypass"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let repo = ione::repos::RelayMappingRepo::new(pool.clone());
    assert_eq!(
        repo.effective_for_user(org, ws, actor).await.unwrap().len(),
        1
    );
    let run = Uuid::new_v4();
    repo.record_run(
        org,
        ws,
        actor,
        run,
        "succeeded",
        &[mapping.id],
        Some(200),
        false,
        None,
        None,
        None,
        json!({}),
    )
    .await
    .unwrap();
    assert!(repo.has_run(org, ws, actor, run).await.unwrap());
    sqlx::query("UPDATE workspace_peer_bindings SET status='inactive' WHERE id=$1")
        .bind(binding)
        .execute(&pool)
        .await
        .unwrap();
    assert!(repo
        .effective_for_user(org, ws, actor)
        .await
        .unwrap()
        .is_empty());
    assert!(!repo.has_run(org, ws, actor, run).await.unwrap());
    assert!(repo
        .set_enabled(org, ws, mapping.id, true)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        client
            .post(format!("{root}/peer-datasets/import"))
            .json(&import)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("DELETE FROM relay_peer_dataset_mappings WHERE mapping_id=$1")
        .bind(mapping.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(repo
        .effective_for_user(org, ws, actor)
        .await
        .unwrap()
        .is_empty());
    assert!(sqlx::query(
        "UPDATE workspace_relay_mappings SET peer_dataset_import=false WHERE id=$1"
    )
    .bind(mapping.id)
    .execute(&pool)
    .await
    .is_err());
    sqlx::query("UPDATE workspace_peer_bindings SET status='active' WHERE id=$1")
        .bind(binding)
        .execute(&pool)
        .await
        .unwrap();
    let revoked_connection = Uuid::new_v4();
    let creating = Arc::new(AtomicBool::new(false));
    let observed = creating.clone();
    Mock::given(method("POST"))
        .and(path("/v1/connections"))
        .and(wiremock::matchers::body_partial_json(
            json!({"name":"Revoked report"}),
        ))
        .respond_with(move |_: &wiremock::Request| {
            observed.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":revoked_connection}))
                .set_delay(std::time::Duration::from_millis(150))
        })
        .with_priority(1)
        .mount(&relay)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/v1/connections/{revoked_connection}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&relay)
        .await;
    let mut revoked_import = import.clone();
    revoked_import["name"] = json!("Revoked report");
    revoked_import["alias"] = json!("revoked_report");
    let in_flight = client
        .post(format!("{root}/peer-datasets/import"))
        .json(&revoked_import);
    let running = tokio::spawn(async move { in_flight.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !creating.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await
        }
    })
    .await
    .unwrap();
    sqlx::query("UPDATE workspace_peer_bindings SET status='inactive' WHERE id=$1")
        .bind(binding)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(running.await.unwrap().status(), 403);
    assert!(repo
        .by_alias(org, ws, "revoked_report")
        .await
        .unwrap()
        .is_none());
    assert!(relay
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.method == "DELETE"
            && r.url.path() == format!("/v1/connections/{revoked_connection}")));
    for table in [
        "relay_source_registrations",
        "relay_peer_dataset_mappings",
        "workspace_relay_mappings",
    ] {
        let rows: Vec<Value> = sqlx::query_scalar(&format!("SELECT to_jsonb(t) FROM {table} t"))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(!serde_json::to_string(&rows).unwrap().contains(&token));
    }
    for request in owner.received_requests().await.unwrap() {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {token}")
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                request
                    .headers
                    .get("x-ione-dataset-origin")
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
            .unwrap(),
            origin
        );
        assert!(request.body.is_empty());
    }
}
