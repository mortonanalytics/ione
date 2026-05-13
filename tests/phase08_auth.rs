/// Phase 8 contract tests — OIDC auth + federated identity + Keycloak.
///
/// These tests are written against:
///   - Contract: md/design/ione-v1-contract.md  (entity `trust_issuer`,
///               `user.oidc_subject`, `membership.federated_claim_ref`)
///   - Plan:     md/plans/ione-v1-plan.md        (Phase 8 scope)
///
/// ALL tests FAIL today because Phase 8 (migration 0008, src/auth.rs,
/// src/routes/auth.rs, IONE_AUTH_MODE, /api/v1/me) does not yet exist.
///
/// ──────────────────────────────────────────────────────────────────────────
/// Prerequisites:
///   docker compose up -d postgres
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione
///
/// Run (serial, ignored):
///   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
///     cargo test --test phase08_auth -- --ignored --test-threads=1
///
/// ──────────────────────────────────────────────────────────────────────────
/// Contract targets (md/design/ione-v1-contract.md):
///
///   trust_issuer fields (JSON camelCase):
///     id, orgId, issuerUrl, audience, jwksUri, claimMapping
///   UNIQUE constraint: (org_id, issuer_url, audience)
///
///   user fields (Phase 8 addition):
///     oidcSubject: TEXT NULL
///
///   membership fields (Phase 8 addition):
///     federatedClaimRef: TEXT NULL
///
///   IONE_AUTH_MODE env: local (default) | oidc
///
///   Endpoints:
///     GET  /auth/login?issuer=<url>              → 302 to issuer authorize URL
///     GET  /auth/callback?code=&state=            → validates state, exchanges code,
///                                                   sets signed session cookie, 302 /
///     POST /auth/logout                           → clears session cookie
///     GET  /api/v1/me                             → { user: User, memberships: Membership[],
///                                                       activeRoleId?: UUID|null }
///
///   /api/v1/health must remain public (no auth required)
///
/// ──────────────────────────────────────────────────────────────────────────
/// Test IDP strategy: in-process mock issuer.
///   Each test that needs a JWT builds it with the `jsonwebtoken` crate signing
///   with a local RSA or HMAC key.  Tests that exercise the HTTP OIDC dance use
///   `ione::auth::handle_test_callback`, a public test hook that bypasses the
///   HTTP round-trip and exercises claim→membership mapping directly.
///   Tests that need a session cookie use `ione::auth::issue_session_cookie`.
///
/// ──────────────────────────────────────────────────────────────────────────
/// All tests are #[ignore]-gated and must be run with --test-threads=1.
/// ──────────────────────────────────────────────────────────────────────────
use std::net::SocketAddr;

use reqwest::{header, redirect::Policy, StatusCode};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

const DEFAULT_DATABASE_URL: &str = "postgres://ione:ione@localhost:5433/ione";
const MOCK_ISSUER_URL: &str = "http://mock-issuer.test";
const MOCK_AUDIENCE: &str = "ione-test";
const MOCK_JWKS_URI: &str = "http://mock-issuer.test/.well-known/jwks.json";

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Connect, run migrations (including 0008 which does not yet exist),
/// truncate in FK-safe order, and boot the server on a random port.
/// Returns `(base_url, pool)`.
async fn spawn_app() -> (String, PgPool) {
    spawn_app_with_auth_mode("local").await
}

/// Like `spawn_app` but sets `IONE_AUTH_MODE` before booting.
async fn spawn_app_with_auth_mode(auth_mode: &str) -> (String, PgPool) {
    // Set the auth mode env var for the server bootstrap path.
    std::env::set_var("IONE_AUTH_MODE", auth_mode);

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to Postgres — is `docker compose up -d postgres` running?");

    sqlx::migrate!("./migrations").run(&pool).await.expect(
        "migration failed — migration 0008 (trust_issuers) may not exist yet \
             (expected failure for contract-red)",
    );

    // Truncate in reverse-FK order including Phase 8 tables.
    // If trust_issuers does not exist yet, this fails — which is the expected
    // contract-red failure mode.
    sqlx::query(
        "TRUNCATE trust_issuers, routing_decisions, survivors, signals, stream_events, streams,
                  connectors, memberships, roles, messages, conversations,
                  workspaces, users, organizations
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate failed — trust_issuers table may not exist yet (expected for contract-red)");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind random port");
    let addr: SocketAddr = listener.local_addr().expect("failed to get local addr");

    // `ione::app(pool)` runs bootstrap (default org + user + Operations workspace + membership).
    let app = ione::app(pool.clone()).await;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server error");
    });

    (format!("http://{}", addr), pool)
}

/// Returns the default org_id for use in trust_issuer inserts.
async fn default_org_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM organizations WHERE name = 'Default Org' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("default org not found — bootstrap seed missing (expected failure)")
}

/// Returns the id of the seeded "Operations" workspace.
async fn ops_workspace_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'Operations' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("Operations workspace not found — bootstrap seed missing (expected failure)")
}

/// Insert a trust_issuer for the mock issuer.  Returns the new row id.
async fn insert_mock_trust_issuer(pool: &PgPool, org_id: Uuid) -> Uuid {
    insert_trust_issuer(pool, org_id, MOCK_ISSUER_URL, MOCK_AUDIENCE, MOCK_JWKS_URI).await
}

async fn insert_trust_issuer(
    pool: &PgPool,
    org_id: Uuid,
    issuer_url: &str,
    audience: &str,
    jwks_uri: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, $2, $3, $4, $5::jsonb)
         RETURNING id",
    )
    .bind(org_id)
    .bind(issuer_url)
    .bind(audience)
    .bind(jwks_uri)
    .bind(json!({
        "role_claim":    "ione_role",
        "coc_level_claim": "ione_coc_level",
        "workspace_name": "Operations"
    }))
    .fetch_one(pool)
    .await
    .expect(
        "insert trust_issuer failed — trust_issuers table may not exist yet \
         (expected failure for contract-red)",
    )
}

async fn issue_db_session_cookie(pool: &PgPool, user_id: Uuid, org_id: Uuid) -> String {
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
    let session_id: Uuid = sqlx::query_scalar(
        "INSERT INTO user_sessions (user_id, org_id, idp_type, expires_at)
         VALUES ($1, $2, 'oidc', $3)
         RETURNING id",
    )
    .bind(user_id)
    .bind(org_id)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .expect("insert user_session failed");
    ione::auth::set_session_cookie_header_for_session(session_id, expires_at)
}

// ─── 1. trust_issuer_unique_constraint ────────────────────────────────────────

/// INSERT the same (org_id, issuer_url, audience) twice → second INSERT must fail
/// with a unique-constraint violation.
///
/// Contract target:
///   - trust_issuer UNIQUE (org_id, issuer_url, audience)
///   - plan Phase 8 migration 0007: UNIQUE(org_id, issuer_url, audience)
///
/// Note: plan labels this 0007; the prompt labels it 0008.  Either way the table
/// must carry this unique constraint.
///
/// REASON: requires DATABASE_URL and migration 0008 (trust_issuers).
#[tokio::test]
#[ignore]
async fn trust_issuer_unique_constraint() {
    let (_base, pool) = spawn_app().await;
    let org_id = default_org_id(&pool).await;

    // First insert succeeds.
    insert_trust_issuer(
        &pool,
        org_id,
        "http://issuer-a.test",
        "aud-a",
        "http://issuer-a.test/jwks",
    )
    .await;

    // Second insert with identical (org_id, issuer_url, audience) must fail.
    let result = sqlx::query(
        "INSERT INTO trust_issuers (org_id, issuer_url, audience, jwks_uri, claim_mapping)
         VALUES ($1, 'http://issuer-a.test', 'aud-a', 'http://issuer-a.test/jwks', '{}'::jsonb)",
    )
    .bind(org_id)
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "second INSERT with same (org_id, issuer_url, audience) must fail with unique constraint \
         violation, but it succeeded — constraint missing (expected failure)"
    );

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("unique") || err_str.contains("duplicate") || err_str.contains("23505"),
        "error must mention unique/duplicate/23505, got: {err_str}"
    );
}

// ─── 2. local_mode_me_returns_default_user ────────────────────────────────────

/// In IONE_AUTH_MODE=local, GET /api/v1/me without any cookie → 200 with
/// user.email=='default@localhost'.
///
/// Contract target:
///   - GET /api/v1/me → { user: User, memberships: Membership[], activeRoleId? }
///   - plan Phase 8: "IONE_AUTH_MODE=local — unauthenticated requests attributed
///     to the seeded default user"
///
/// REASON: requires src/routes/auth.rs (or equivalent) with GET /api/v1/me,
///         and IONE_AUTH_MODE env support.
#[tokio::test]
#[ignore]
async fn local_mode_me_returns_default_user() {
    let (base, _pool) = spawn_app_with_auth_mode("local").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/me", base))
        .send()
        .await
        .expect("GET /api/v1/me failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/me in local mode, got {} \
         (route not registered — expected failure)",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body not JSON");

    // user object must exist
    let user = &body["user"];
    assert!(
        !user.is_null(),
        "GET /api/v1/me must return a 'user' field, got: {body}"
    );

    // email must be default@localhost
    assert_eq!(
        user["email"], "default@localhost",
        "local mode must return default@localhost user, got email: {}",
        user["email"]
    );

    // memberships array must be present
    assert!(
        body["memberships"].is_array(),
        "GET /api/v1/me must return a 'memberships' array, got: {body}"
    );

    // activeRoleId field must be present (may be null)
    assert!(
        body.get("activeRoleId").is_some(),
        "GET /api/v1/me must include 'activeRoleId' key (may be null), got: {body}"
    );
}

// ─── 3. oidc_login_redirects_to_issuer ────────────────────────────────────────

/// In IONE_AUTH_MODE=oidc, GET /auth/login?issuer=<url> → 302; Location contains
/// `state` and `code_challenge` query params.
///
/// Contract target:
///   - GET /auth/login?issuer=<url> → 302 to issuer authorize URL
///   - plan Phase 8: "redirect to OIDC issuer with state + code_challenge"
///
/// REASON: requires src/routes/auth.rs with GET /auth/login and oidc mode.
#[tokio::test]
#[ignore]
async fn oidc_login_redirects_to_issuer() {
    let (base, pool) = spawn_app_with_auth_mode("oidc").await;
    let org_id = default_org_id(&pool).await;

    // Register the mock issuer so the server can look it up.
    insert_mock_trust_issuer(&pool, org_id).await;

    // Use a non-redirecting client so we can inspect the 302 directly.
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("failed to build non-redirect client");

    let resp = client
        .get(format!(
            "{}/auth/login?issuer={}",
            base,
            urlencoding::encode(MOCK_ISSUER_URL)
        ))
        .send()
        .await
        .expect("GET /auth/login failed");

    assert_eq!(
        resp.status(),
        StatusCode::FOUND,
        "GET /auth/login must return 302 in oidc mode, got {} \
         (route not registered or oidc mode not implemented — expected failure)",
        resp.status()
    );

    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("302 response must include a Location header");

    // Location must include `state` param (CSRF protection)
    assert!(
        location.contains("state="),
        "Location header must contain 'state=' param (CSRF token), got: {location}"
    );

    // Location must include `code_challenge` (PKCE)
    assert!(
        location.contains("code_challenge="),
        "Location header must contain 'code_challenge=' param (PKCE), got: {location}"
    );
}

// ─── 4. oidc_callback_rejects_bad_state ───────────────────────────────────────

/// GET /auth/callback?code=xxx&state=invalid (without matching state cookie) → 400.
///
/// Contract target:
///   - GET /auth/callback validates state cookie before exchanging code
///   - plan Phase 8: "validates state cookie"
///
/// REASON: requires src/routes/auth.rs with GET /auth/callback.
#[tokio::test]
#[ignore]
async fn oidc_callback_rejects_bad_state() {
    let (base, _pool) = spawn_app_with_auth_mode("oidc").await;
    let client = reqwest::Client::new();

    // Submit callback without a state cookie; state param is garbage.
    let resp = client
        .get(format!(
            "{}/auth/callback?code=authcode-xxx&state=invalid-state-no-cookie",
            base
        ))
        .send()
        .await
        .expect("GET /auth/callback failed");

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 401 || status == 422,
        "GET /auth/callback with invalid/missing state cookie must return 4xx, got {} \
         (route not registered — expected failure)",
        status
    );
}

// ─── 5. oidc_callback_with_signed_id_token_creates_user_and_membership ────────

/// Use the test hook `ione::auth::handle_test_callback(pool, issuer_url, id_token_claims)`
/// to exercise claim→membership mapping without an HTTP round-trip.
///
/// Claims: {sub:"user-1", email:"jsmith@test", name:"J Smith",
///          ione_role:"member", ione_coc_level:0}
///
/// Asserts:
///   - users row with oidc_subject='user-1', email='jsmith@test' exists
///   - memberships row joining that user to the Operations workspace's 'member'
///     role exists
///   - membership.federated_claim_ref is non-null (e.g., "user-1@http://mock-issuer.test")
///
/// Contract targets:
///   - user.oidcSubject (contract § user)
///   - membership.federatedClaimRef (contract § membership)
///   - plan Phase 8: "upserts user by oidc_subject, upserts membership via claim_mapping"
///
/// REASON: requires src/auth.rs (or src/routes/auth.rs) with a public
///         `ione::auth::handle_test_callback` fn; requires migration 0008.
#[tokio::test]
#[ignore]
async fn oidc_callback_with_signed_id_token_creates_user_and_membership() {
    let (_base, pool) = spawn_app_with_auth_mode("oidc").await;
    let org_id = default_org_id(&pool).await;
    let _issuer_id = insert_mock_trust_issuer(&pool, org_id).await;

    let claims = json!({
        "sub":             "user-1",
        "email":           "jsmith@test",
        "name":            "J Smith",
        "ione_role":       "member",
        "ione_coc_level":  0
    });

    // This test hook bypasses the HTTP dance and directly exercises claim mapping.
    // It must: look up the trust_issuer, upsert users, upsert memberships.
    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims)
        .await
        .expect(
            "handle_test_callback must succeed for valid claims \
             (not implemented — expected failure)",
        );

    // --- users row ---
    let user_row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, email, oidc_subject FROM users WHERE oidc_subject = 'user-1' LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .expect("user query failed");

    let user_row = user_row.expect(
        "users row with oidc_subject='user-1' must exist after handle_test_callback \
         (expected failure — handle_test_callback not implemented)",
    );

    assert_eq!(
        user_row.1, "jsmith@test",
        "user.email must be 'jsmith@test', got: {}",
        user_row.1
    );
    assert_eq!(
        user_row.2.as_deref(),
        Some("user-1"),
        "user.oidc_subject must be 'user-1', got: {:?}",
        user_row.2
    );

    let user_id = user_row.0;

    // --- memberships row ---
    let membership_row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT m.id, r.name, m.federated_claim_ref
         FROM memberships m
         JOIN roles r ON r.id = m.role_id
         JOIN workspaces w ON w.id = m.workspace_id
         WHERE m.user_id = $1
           AND w.name = 'Operations'
           AND r.name = 'member'
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&pool)
    .await
    .expect("membership query failed");

    let membership_row = membership_row.expect(
        "memberships row for jsmith@test × Operations × member must exist \
         after handle_test_callback (expected failure)",
    );

    // federated_claim_ref must be non-null
    assert!(
        membership_row.2.is_some(),
        "membership.federated_claim_ref must be non-null after OIDC login, got None"
    );

    let claim_ref = membership_row.2.unwrap();
    assert!(
        !claim_ref.is_empty(),
        "membership.federated_claim_ref must be non-empty, got empty string"
    );

    // The claim_ref format is "sub@issuer_url" per the spec example.
    assert!(
        claim_ref.contains("user-1"),
        "federated_claim_ref must embed the subject 'user-1', got: {claim_ref}"
    );
}

// ─── 6. oidc_callback_repeated_is_idempotent ──────────────────────────────────

/// Calling handle_test_callback with the same claims twice must result in
/// exactly one users row and one memberships row (upsert semantics).
///
/// Contract targets:
///   - plan Phase 8: "upserts User by oidc_subject, upserts Membership via claim_mapping"
///   - user.oidcSubject UNIQUE implies idempotency
///
/// REASON: requires ione::auth::handle_test_callback.
#[tokio::test]
#[ignore]
async fn oidc_callback_repeated_is_idempotent() {
    let (_base, pool) = spawn_app_with_auth_mode("oidc").await;
    let org_id = default_org_id(&pool).await;
    let _issuer_id = insert_mock_trust_issuer(&pool, org_id).await;

    let claims = json!({
        "sub":            "idempotent-user",
        "email":          "idempotent@test",
        "name":           "Idempotent User",
        "ione_role":      "member",
        "ione_coc_level": 0
    });

    // First call
    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims.clone())
        .await
        .expect("first handle_test_callback must succeed (expected failure)");

    // Second call — must not create duplicates
    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims)
        .await
        .expect("second handle_test_callback must succeed (idempotency expected failure)");

    // Exactly one user
    let user_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE oidc_subject = 'idempotent-user'")
            .fetch_one(&pool)
            .await
            .expect("user count query failed");

    assert_eq!(
        user_count, 1,
        "calling handle_test_callback twice with same claims must produce exactly 1 users row, \
         got {}",
        user_count
    );

    // Exactly one membership
    let membership_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM memberships m
         JOIN users u ON u.id = m.user_id
         WHERE u.oidc_subject = 'idempotent-user'",
    )
    .fetch_one(&pool)
    .await
    .expect("membership count query failed");

    assert_eq!(
        membership_count, 1,
        "calling handle_test_callback twice with same claims must produce exactly 1 \
         memberships row, got {}",
        membership_count
    );
}

// ─── 7. me_with_valid_session_returns_oidc_user ───────────────────────────────

/// After handle_test_callback, use the test helper
/// `ione::auth::issue_session_cookie(user_id)` to simulate a signed session
/// cookie; GET /api/v1/me with that cookie → returns the oidc user, not the
/// default user.
///
/// Contract targets:
///   - GET /api/v1/me → { user: User, memberships: Membership[], activeRoleId? }
///   - plan Phase 8: "middleware derives AuthContext from cookie; handlers use
///     AuthContext.user_id"
///   - user.email must NOT be 'default@localhost' when authenticated as oidc user
///
/// REASON: requires ione::auth::handle_test_callback,
///         ione::auth::issue_session_cookie, and GET /api/v1/me.
#[tokio::test]
#[ignore]
async fn me_with_valid_session_returns_oidc_user() {
    let (base, pool) = spawn_app_with_auth_mode("oidc").await;
    let org_id = default_org_id(&pool).await;
    insert_mock_trust_issuer(&pool, org_id).await;

    let claims = json!({
        "sub":            "session-user-1",
        "email":          "session-user@test",
        "name":           "Session User",
        "ione_role":      "member",
        "ione_coc_level": 0
    });

    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims)
        .await
        .expect("handle_test_callback failed (expected failure)");

    // Retrieve the user id we just upserted
    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = 'session-user-1' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("user not found after handle_test_callback");

    // Mint a signed DB-backed session cookie for that user.
    let cookie_value = issue_db_session_cookie(&pool, user_id, org_id).await;

    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/me", base))
        .header(header::COOKIE, cookie_value)
        .send()
        .await
        .expect("GET /api/v1/me with cookie failed");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200 from GET /api/v1/me with valid session cookie, got {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("body not JSON");
    let user = &body["user"];

    assert_eq!(
        user["email"], "session-user@test",
        "GET /api/v1/me with valid cookie must return the oidc user, \
         got email: {}",
        user["email"]
    );

    // Must NOT be the default user
    assert_ne!(
        user["email"], "default@localhost",
        "GET /api/v1/me with valid session cookie must NOT return the default user"
    );

    // oidcSubject must be present
    assert_eq!(
        user["oidcSubject"], "session-user-1",
        "GET /api/v1/me user.oidcSubject must be 'session-user-1', got: {}",
        user["oidcSubject"]
    );
}

// ─── 8. logout_clears_session ─────────────────────────────────────────────────

/// POST /auth/logout with a valid cookie → 200 or 204, response sets an expired
/// cookie; subsequent /api/v1/me returns the default user (local-fallback
/// semantics for air-gap compatibility).
///
/// Contract target:
///   - POST /auth/logout → clears session cookie
///   - plan Phase 8: "POST /auth/logout → clears cookie"
///   - air-gap semantics: after logout, local mode falls back to default user
///
/// REASON: requires src/routes/auth.rs with POST /auth/logout,
///         ione::auth::issue_session_cookie.
#[tokio::test]
#[ignore]
async fn logout_clears_session() {
    let (base, pool) = spawn_app_with_auth_mode("local").await;
    let org_id = default_org_id(&pool).await;
    insert_mock_trust_issuer(&pool, org_id).await;

    let claims = json!({
        "sub":            "logout-user-1",
        "email":          "logout-user@test",
        "name":           "Logout User",
        "ione_role":      "member",
        "ione_coc_level": 0
    });

    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims)
        .await
        .expect("handle_test_callback failed (expected failure)");

    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = 'logout-user-1' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("user not found after handle_test_callback");

    let cookie_value = ione::auth::issue_session_cookie(user_id)
        .await
        .expect("issue_session_cookie failed (expected failure)");

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("failed to build cookie-storing client");

    // Logout
    let logout_resp = client
        .post(format!("{}/auth/logout", base))
        .header(header::COOKIE, cookie_value)
        .send()
        .await
        .expect("POST /auth/logout failed");

    let logout_status = logout_resp.status().as_u16();
    assert!(
        logout_status == 200 || logout_status == 204,
        "POST /auth/logout must return 200 or 204, got {} \
         (route not registered — expected failure)",
        logout_status
    );

    // The logout response must set a Set-Cookie header that expires the session.
    let set_cookie = logout_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect(
            "POST /auth/logout must set a Set-Cookie header to clear the session \
             (expected failure)",
        );

    // The cookie must be expired: Max-Age=0 or Expires in the past.
    let cookie_lower = set_cookie.to_lowercase();
    assert!(
        cookie_lower.contains("max-age=0")
            || cookie_lower.contains("expires=thu, 01 jan 1970")
            || cookie_lower.contains("max-age=-"),
        "logout Set-Cookie must expire the session (Max-Age=0 or past Expires), \
         got: {set_cookie}"
    );

    // After logout, /api/v1/me without a cookie should fall back to the default user.
    let me_resp = client
        .get(format!("{}/api/v1/me", base))
        .send()
        .await
        .expect("GET /api/v1/me after logout failed");

    assert_eq!(
        me_resp.status(),
        StatusCode::OK,
        "GET /api/v1/me after logout must return 200 (fallback to local user), got {}",
        me_resp.status()
    );

    let me_body: Value = me_resp.json().await.expect("body not JSON");
    assert_eq!(
        me_body["user"]["email"], "default@localhost",
        "GET /api/v1/me after logout must return the default local user, \
         got email: {}",
        me_body["user"]["email"]
    );
}

// ─── 9. claim_mapping_custom_role_creates_correct_membership ──────────────────

/// Claims `{ione_role:"duty_officer", ione_coc_level:1}` with an issuer whose
/// claim_mapping maps ione_role → role_name and ione_coc_level → coc_level.
///
/// Assert the membership resolves to a role named "duty_officer" at coc_level=1
/// scoped to the Operations workspace.  Per the plan, the role is auto-created
/// if missing; we also accept it must pre-exist if the implementation documents
/// that choice.
///
/// Contract targets:
///   - trust_issuer.claimMapping (contract § trust_issuer)
///   - membership.roleId (resolved via claim_mapping)
///   - role.cocLevel (contract § role)
///   - plan Phase 8: "resolves/creates the membership for the mapped role"
///
/// REASON: requires ione::auth::handle_test_callback with claim_mapping support.
#[tokio::test]
#[ignore]
async fn claim_mapping_custom_role_creates_correct_membership() {
    let (_base, pool) = spawn_app_with_auth_mode("oidc").await;
    let org_id = default_org_id(&pool).await;
    let ops_ws_id = ops_workspace_id(&pool).await;

    // Register an issuer with claim_mapping that supports duty_officer.
    insert_trust_issuer(
        &pool,
        org_id,
        "http://custom-issuer.test",
        MOCK_AUDIENCE,
        "http://custom-issuer.test/jwks",
    )
    .await;

    let claims = json!({
        "sub":            "duty-officer-user",
        "email":          "do@test",
        "name":           "Duty Officer",
        "ione_role":      "duty_officer",
        "ione_coc_level": 1
    });

    let _: () = ione::auth::handle_test_callback(&pool, "http://custom-issuer.test", claims)
        .await
        .expect("handle_test_callback failed for duty_officer claims (expected failure)");

    // Find the user
    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = 'duty-officer-user' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("user not found after handle_test_callback");

    // Find the membership and joined role
    let row: Option<(String, i32)> = sqlx::query_as(
        "SELECT r.name, r.coc_level
         FROM memberships m
         JOIN roles r ON r.id = m.role_id
         WHERE m.user_id = $1
           AND m.workspace_id = $2
         LIMIT 1",
    )
    .bind(user_id)
    .bind(ops_ws_id)
    .fetch_optional(&pool)
    .await
    .expect("membership/role query failed");

    let (role_name, coc_level) = row.expect(
        "membership for duty_officer claims must exist in Operations workspace \
         after handle_test_callback (expected failure)",
    );

    assert_eq!(
        role_name, "duty_officer",
        "membership must link to a role named 'duty_officer', got: {role_name}"
    );

    assert_eq!(
        coc_level, 1,
        "role.coc_level must be 1 for ione_coc_level=1 claim, got: {coc_level}"
    );
}

// ─── 10. trust_issuer_must_exist_for_login ────────────────────────────────────

/// GET /auth/login?issuer=http://unknown → 400 or 404.
///
/// Contract target:
///   - GET /auth/login requires the issuer to exist in trust_issuers for the org
///   - plan Phase 8: server looks up trust_issuer on login request
///
/// REASON: requires src/routes/auth.rs with GET /auth/login.
#[tokio::test]
#[ignore]
async fn trust_issuer_must_exist_for_login() {
    let (base, _pool) = spawn_app_with_auth_mode("oidc").await;

    // Do NOT register any trust_issuer; use an unknown issuer URL.
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("failed to build non-redirect client");

    let resp = client
        .get(format!(
            "{}/auth/login?issuer={}",
            base,
            urlencoding::encode("http://unknown-issuer.test")
        ))
        .send()
        .await
        .expect("GET /auth/login failed");

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 404,
        "GET /auth/login with unknown issuer must return 400 or 404, got {} \
         (route not registered — expected failure)",
        status
    );
}

// ─── 11. expired_session_cookie_is_rejected ───────────────────────────────────

/// `issue_session_cookie_with_expiry(user_id, past_time)` produces a cookie
/// whose session is expired; GET /api/v1/me with that cookie falls back to the
/// local default user (air-gap semantics), or returns 401.
///
/// Contract target:
///   - plan Phase 8: expired sessions treated as unauthenticated; in local mode
///     that means fall back to the default user
///
/// REASON: requires ione::auth::issue_session_cookie_with_expiry and GET /api/v1/me.
#[tokio::test]
#[ignore]
async fn expired_session_cookie_is_rejected() {
    let (base, pool) = spawn_app_with_auth_mode("local").await;
    let org_id = default_org_id(&pool).await;
    insert_mock_trust_issuer(&pool, org_id).await;

    let claims = json!({
        "sub":            "expire-test-user",
        "email":          "expired@test",
        "name":           "Expire Test",
        "ione_role":      "member",
        "ione_coc_level": 0
    });

    let _: () = ione::auth::handle_test_callback(&pool, MOCK_ISSUER_URL, claims)
        .await
        .expect("handle_test_callback failed (expected failure)");

    let user_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE oidc_subject = 'expire-test-user' LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("user not found");

    // Mint a cookie that expired one hour ago.
    let past_time = chrono::Utc::now() - chrono::Duration::hours(1);
    let expired_cookie = ione::auth::issue_session_cookie_with_expiry(user_id, past_time)
        .await
        .expect("issue_session_cookie_with_expiry failed (not implemented — expected failure)");

    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/me", base))
        .header(header::COOKIE, expired_cookie)
        .send()
        .await
        .expect("GET /api/v1/me with expired cookie failed");

    // In local mode: expired session falls back to the default user.
    // Accept 401 as well (stricter oidc-mode behavior).
    let status = resp.status().as_u16();
    if status == 401 {
        // Strict rejection is acceptable.
        return;
    }

    assert_eq!(
        status, 200,
        "GET /api/v1/me with expired cookie must return 200 (fallback) or 401, got {}",
        status
    );

    let body: Value = resp.json().await.expect("body not JSON");
    assert_eq!(
        body["user"]["email"], "default@localhost",
        "expired cookie in local mode must fall back to default@localhost, \
         got email: {}",
        body["user"]["email"]
    );
}

// ─── 12. phase_1_health_still_public ─────────────────────────────────────────

/// GET /api/v1/health works without any auth in both local and oidc mode.
///
/// This is a Phase 1 regression: adding the auth middleware must not break
/// the public health endpoint.
///
/// Contract target:
///   - GET /api/v1/health → { status: "ok", version } (contract § API operations)
///   - plan Phase 8: "middleware applied to every non-auth route" — health must
///     be exempted or explicitly public
///
/// REASON: requires the Phase 8 auth middleware to be in place (or NOT break
///         the health route).  Fails today if the middleware blocks it.
#[tokio::test]
#[ignore]
async fn phase_1_health_still_public() {
    // Test in local mode
    {
        let (base, _pool) = spawn_app_with_auth_mode("local").await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("{}/api/v1/health", base))
            .send()
            .await
            .expect("GET /api/v1/health (local mode) failed");

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "GET /api/v1/health must return 200 in local mode, got {}",
            resp.status()
        );

        let body: Value = resp.json().await.expect("health body not JSON");
        assert_eq!(
            body["status"], "ok",
            "health response must have status='ok', got: {}",
            body["status"]
        );
        assert!(
            !body["version"].is_null(),
            "health response must include a 'version' field, got: {body}"
        );
    }

    // Test in oidc mode — health must remain public even when auth is enforced.
    {
        let (base, _pool) = spawn_app_with_auth_mode("oidc").await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("{}/api/v1/health", base))
            .send()
            .await
            .expect("GET /api/v1/health (oidc mode) failed");

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "GET /api/v1/health must return 200 in oidc mode (must not require auth), got {}",
            resp.status()
        );
    }
}

// ─── Mutation check (documented per contract-red protocol) ───────────────────
//
// These tests can't run mutations against an unimplemented codebase, but the
// following mutations are documented to verify test strength once implementation
// exists:
//
// - Mutant: remove UNIQUE(org_id, issuer_url, audience) from migration
//   → caught by trust_issuer_unique_constraint ✓
//
// - Mutant: return default user even when valid session cookie is present
//   → caught by me_with_valid_session_returns_oidc_user (asserts email != default@localhost) ✓
//
// - Mutant: skip setting oidc_subject on user upsert
//   → caught by oidc_callback_with_signed_id_token_creates_user_and_membership
//     (asserts user.oidc_subject == 'user-1') ✓
//
// - Mutant: skip setting federated_claim_ref on membership upsert
//   → caught by oidc_callback_with_signed_id_token_creates_user_and_membership
//     (asserts federated_claim_ref is_some() and contains "user-1") ✓
//
// - Mutant: remove idempotency guard from membership upsert (insert instead of upsert)
//   → caught by oidc_callback_repeated_is_idempotent (asserts COUNT(*) == 1) ✓
//
// - Mutant: map all claim roles to 'member' regardless of ione_role claim
//   → caught by claim_mapping_custom_role_creates_correct_membership
//     (asserts role_name == 'duty_officer') ✓
//
// - Mutant: always redirect login regardless of whether issuer is registered
//   → caught by trust_issuer_must_exist_for_login (asserts 4xx) ✓
//
// - Mutant: accept any state param in callback without checking the cookie
//   → caught by oidc_callback_rejects_bad_state (asserts 4xx without matching cookie) ✓
//
// - Mutant: logout response omits Set-Cookie header
//   → caught by logout_clears_session (asserts Set-Cookie with Max-Age=0 present) ✓
//
// - Mutant: health returns 401 when auth middleware covers all routes
//   → caught by phase_1_health_still_public ✓
