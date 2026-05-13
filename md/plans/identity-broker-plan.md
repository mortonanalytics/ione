# Identity Broker — Implementation Plan

**Design doc:** [md/design/identity-broker.md](../design/identity-broker.md)
**Shape:** large
**Stack:** Rust 1.78+ / Axum / sqlx (Postgres) / vanilla-JS static frontend. No TypeScript compile gate; UI verification is integration-test + browser smoke.

**Revision history:**
- 2026-05-12 initial plan written from design doc.
- 2026-05-12 revised after Codex plan-and-contract review. Eight findings + two policy gaps folded in: session-id cookie payload migration, RLS context middleware (`rls_context.rs` + transaction-bound `TxHandle`), `enforce_auth` changed from extractor to middleware, public `GET /auth/issuers` endpoint (no longer borrows admin endpoint), subject-based federated identity binding (`federated_identities` table, not email-keyed user lookup), MFA two-axis enforcement matrix (admins must enroll when org flag is set, not just challenge if already enrolled), logout server-side revocation added to Phase 1, broker `state_token` uniqueness + replay/expired ACs + `UNIQUE(user_id, provider, label)` instead of `(user_id, provider)`, audit rows on admin mutations, v0.1-known-limitations section for rate-limiting deferral.

## Dependencies (new crates)

| Crate | Version | Purpose |
|---|---|---|
| `openidconnect` | `"3"` | OIDC consumer: discovery, JWKS, ID-token validation (nonce/aud/iss/exp/iat). Replaces handwritten path that the security review flagged. |
| `totp-lite` | `"2"` | TOTP code generation and verification with clock-skew tolerance. |
| `argon2` | `"0.5"` | Hashing for MFA recovery codes (one-time use, never plaintext). |
| `qrcode` | `"0.14"` | TOTP enrollment QR rendering. `default-features = false` (no PNG dep). |
| `data-encoding` | `"2"` | Base32 encoding for TOTP secret display. |

Already present and reused: `aes-gcm`, `hmac`, `sha2`, `rand`, `base64`, `jsonwebtoken`, `subtle` (constant-time compare for nonces), `reqwest`, `sqlx`, `axum`.

**Out-of-tree (deployed beside IONe in the rare on-prem-SAML case):** Keycloak as SAML→OIDC bridge. No code change in IONe; documented in operator deployment guide only.

## Migration numbering

Latest existing migration: `0017_peer_token_ciphertext.sql`. New migrations claim `0018..0024` in phase order below. SQL filenames follow existing convention `00000000000018_<slug>.sql` (sqlx naming).

## Phases

Phases are vertical slices. Each ends in a runnable gate.

---

### Phase 0 — Prerequisites (P-1, P-2, P-3 from the design)

**Goal:** close three pre-existing security blockers so the broker can be built on hardened foundations. Three independent fixes; can be coded in parallel.

**Files:**
- `migrations/00000000000018_peer_oauth_pending.sql` — **create**
- `src/routes/peers.rs` — **modify** (replace `PENDING_FEDERATIONS` in-memory map with DB-backed pending table)
- `src/auth.rs` — **modify** (add `enforce_auth` extractor; return 401 in OIDC mode when session absent)
- `src/routes/mod.rs` — **modify** (apply `enforce_auth` to protected routes; replace `allow_origin(Any)` with `IONE_CORS_ALLOWED_ORIGINS` env-driven allowlist)

**Code shapes:**

```sql
-- 0018_peer_oauth_pending.sql
CREATE TABLE peer_oauth_pending (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    peer_id       UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
    nonce         TEXT NOT NULL UNIQUE,
    code_verifier TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX peer_oauth_pending_expires ON peer_oauth_pending(expires_at);
```

```rust
// src/routes/peers.rs — begin_federation
let nonce = base64_url::encode(&rand::random::<[u8; 32]>());
let code_verifier = generate_pkce_verifier();
sqlx::query!(
    "INSERT INTO peer_oauth_pending (peer_id, nonce, code_verifier, expires_at) \
     VALUES ($1, $2, $3, now() + interval '10 minutes')",
    peer_id, &nonce, &code_verifier
).execute(&pool).await?;
// authorize URL state = nonce, NOT peer_id

// src/routes/peers.rs — callback
let row = sqlx::query!(
    "DELETE FROM peer_oauth_pending WHERE nonce = $1 AND expires_at > now() RETURNING peer_id, code_verifier",
    &state_nonce
).fetch_optional(&pool).await?
    .ok_or(AppError::BadRequest("invalid or expired state".into()))?;
// constant-time check on the load by using subtle::ConstantTimeEq if exact nonce returned; SQL = is fine because we delete-by-equality
```

```rust
// src/auth.rs — Axum middleware (NOT an extractor; extractors only fire when handlers ask for them)
pub async fn enforce_auth(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, AppError> {
    let ctx = req.extensions().get::<AuthContext>().cloned();
    let mode = req.extensions().get::<AuthMode>().copied();
    if matches!(mode, Some(AuthMode::Oidc)) {
        match ctx {
            Some(c) if c.is_oidc => {}
            _ => return Err(AppError::Unauthorized),
        }
    }
    Ok(next.run(req).await)
}

// src/routes/mod.rs — apply unconditionally to the protected subtree
let protected = Router::new()
    .nest("/api/v1", api_v1_routes())
    .layer(axum::middleware::from_fn(enforce_auth))
    .layer(axum::middleware::from_fn_with_state(state.clone(), auth_middleware));
```

```rust
// src/routes/mod.rs — CORS
let allowed = std::env::var("IONE_CORS_ALLOWED_ORIGINS")
    .unwrap_or_default()
    .split(',').filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
let cors = if allowed.is_empty() {
    CorsLayer::new()  // deny by default
} else {
    CorsLayer::new()
        .allow_origin(allowed.into_iter().map(|s| s.parse().unwrap()).collect::<Vec<_>>())
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::PUT])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
};
```

**Gate:**
```bash
cargo sqlx migrate run && cargo clippy --all-targets -- -D warnings && cargo test --test phase11_peer -- --ignored --test-threads=1
```

**Acceptance:**
- `psql -c "\d peer_oauth_pending"` shows the new table.
- `curl -sS -o /dev/null -w "%{http_code}" http://localhost:3002/api/v1/workspaces` returns `401` when `IONE_AUTH_MODE=oidc` and no session cookie.
- `curl -H "Origin: https://evil.example" -I http://localhost:3002/api/v1/workspaces` does NOT echo `Access-Control-Allow-Origin: https://evil.example`.
- Peer federation round-trip still completes against a local mock IdP (existing test).

---

### Phase 1 — DB-backed sessions + audit-event infrastructure + RLS context (S1 + S6 skeleton)

**Goal:** every subsequent phase emits audit rows, uses revocable DB sessions, and runs queries under a per-request org context that RLS can enforce against.

**Files:**
- `migrations/00000000000019_user_sessions_audit.sql` — **create**
- `src/repos/user_session_repo.rs` — **create**
- `src/services/session_service.rs` — **create**
- `src/services/identity_audit_writer.rs` — **create**
- `src/middleware/rls_context.rs` — **create** (per-request RLS context wrapper; see below)
- `src/auth.rs` — **modify** (DB session lookup in middleware; add `session_id` and `mfa_verified` to `AuthContext`; **change cookie payload from `user_id:exp` to `session_id:exp`**; update `sign_payload`/parser callers accordingly)
- `src/routes/auth_routes.rs` — **modify** (logout calls `SessionService::revoke(session_id)`, writes `SessionRevoke` audit event)
- `src/repos/mod.rs` — **modify** (re-export `UserSessionRepo`)
- `src/services/mod.rs` — **modify** (re-export `SessionService`, `IdentityAuditWriter`)
- `src/models/mod.rs` — **modify** (`UserSession` struct)

**Code shapes:**

```sql
-- 0019_user_sessions_audit.sql
CREATE TABLE user_sessions (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES organizations(id),
    idp_type     TEXT NOT NULL,                                       -- 'local' | 'oidc'
    mfa_verified BOOLEAN NOT NULL DEFAULT false,
    expires_at   TIMESTAMPTZ NOT NULL,
    revoked_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX user_sessions_user ON user_sessions(user_id);
CREATE INDEX user_sessions_active ON user_sessions(expires_at) WHERE revoked_at IS NULL;

-- user_sessions intentionally does NOT have RLS. The session_id itself is the unguessable
-- auth credential (signed in the cookie); RLS would create a chicken-and-egg with org context
-- resolution. RLS lives on every downstream table (mfa_*, broker_*, identity_audit_events,
-- etc.) that holds org-scoped data.

CREATE TABLE identity_audit_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    event_type  TEXT NOT NULL,
    org_id      UUID NOT NULL REFERENCES organizations(id),
    user_id     UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_ip    INET,
    session_id  UUID REFERENCES user_sessions(id) ON DELETE SET NULL,
    peer_id     UUID REFERENCES peers(id) ON DELETE SET NULL,
    outcome     TEXT NOT NULL,                                        -- 'success' | 'failure' | 'denied'
    detail      JSONB
);
CREATE INDEX identity_audit_org_occurred ON identity_audit_events(org_id, occurred_at DESC);

ALTER TABLE identity_audit_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY identity_audit_org_isolation ON identity_audit_events
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
```

```rust
// src/services/session_service.rs
pub struct SessionService<'a> { pool: &'a PgPool, audit: &'a IdentityAuditWriter<'a> }

impl<'a> SessionService<'a> {
    pub async fn create(&self, user_id: Uuid, org_id: Uuid, idp_type: &str)
        -> anyhow::Result<(Uuid, String /* set-cookie header value carrying session_id:exp */)> { ... }
    pub async fn revoke(&self, session_id: Uuid) -> anyhow::Result<()> { ... }
    pub async fn mark_mfa_verified(&self, session_id: Uuid) -> anyhow::Result<()> { ... }
    pub async fn find_active(&self, session_id: Uuid) -> anyhow::Result<Option<UserSession>> { ... }
}

// src/middleware/rls_context.rs — RLS context wrapper used by every protected handler.
// auth_middleware runs first (does NOT need RLS — it queries user_sessions which has no RLS).
// After AuthContext is populated, this middleware opens a transaction and sets app.current_org_id
// for the rest of the request. Handlers receive a transaction-bound connection on extensions.
pub async fn rls_context_layer(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, AppError> {
    let ctx = req.extensions().get::<AuthContext>()
        .ok_or(AppError::Unauthorized)?.clone();
    let mut tx = state.pool.begin().await.map_err(AppError::Internal)?;
    sqlx::query("SELECT set_config('app.current_org_id', $1, true)")
        .bind(ctx.org_id.to_string())
        .execute(&mut *tx).await.map_err(AppError::Internal)?;
    // Attach the transaction handle to request extensions so handlers can use it.
    // Commit at response time.
    let mut req = req;
    req.extensions_mut().insert(TxHandle::new(tx));
    let resp = next.run(req).await;
    // TxHandle has a Drop impl that commits on success, rolls back on AppError 5xx — implemented
    // in the rls_context module.
    Ok(resp)
}
// Handler usage: `axum::Extension(tx): axum::Extension<TxHandle>` then `tx.acquire().await`.
// Repository methods take `&mut PgConnection` (already the pattern in src/repos/), so this
// integrates with existing query call sites.

// src/services/identity_audit_writer.rs
#[derive(Clone, Copy)]
pub enum IdentityEvent {
    // session lifecycle
    OidcLogin, OidcLoginFailure, Logout, SessionRevoke,
    // mfa
    MfaEnroll, MfaVerify, MfaFail, MfaDisable, MfaRecoveryConsume, MfaRecoveryViewed,
    // brokered SaaS tokens
    TokenBrokerGrant, TokenBrokerRefresh, TokenBrokerRevoke, TokenBrokerRevokeUpstreamFailed,
    // identity-control admin mutations (Codex policy-gap A)
    TrustIssuerCreate, TrustIssuerDelete,
}

pub struct IdentityAuditWriter<'a> { pool: &'a PgPool }
impl<'a> IdentityAuditWriter<'a> {
    pub async fn write(&self,
        event: IdentityEvent, org_id: Uuid, user_id: Option<Uuid>,
        session_id: Option<Uuid>, peer_id: Option<Uuid>,
        actor_ip: Option<IpAddr>, outcome: &str, detail: serde_json::Value,
    ) -> anyhow::Result<()> { ... }
}

// src/auth.rs — AuthContext additions
pub struct AuthContext {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub is_oidc: bool,
    pub is_mcp_peer: bool,
    pub active_role_id: Option<Uuid>,
    pub session_id: Option<Uuid>,   // new — None for local/MCP-peer/legacy fallback
    pub mfa_verified: bool,         // new — populated from user_sessions row
}
```

**Gate:**
```bash
cargo sqlx migrate run && cargo clippy --all-targets -- -D warnings && cargo test session -- --nocapture
```

**Acceptance:**
- `psql -c "\dt user_sessions identity_audit_events"` shows both tables.
- Integration test: create session via `SessionService::create`, revoke it, then a request carrying the cookie returns 401 with body `error: "unauthorized"`.
- Integration test: `POST /auth/logout` with a valid session cookie sets `user_sessions.revoked_at` non-null and writes an `identity_audit_events` row with `event_type='logout'`.
- Integration test: cookie now carries `<session_id>:<exp>`, not `<user_id>:<exp>` — a cookie minted before this migration is treated as invalid (forces re-login).
- Integration test: under RLS context middleware, a request running as org A cannot SELECT an `identity_audit_events` row belonging to org B even via a raw query through the request's `TxHandle`.

---

### Phase 2 — OIDC consumer with Microsoft Entra ID as default IdP (S2)

**Goal:** real OIDC round-trip against Entra ID (and any other standards-compliant IdP). Replaces the stub callback.

**Files:**
- `migrations/00000000000020_trust_issuers_oidc.sql` — **create** (extend `trust_issuers`)
- `src/services/idp_service.rs` — **create**
- `src/services/claim_mapper.rs` — **create** (extract from `src/auth.rs`)
- `src/routes/auth_routes.rs` — **modify** (replace stub callback, add `?issuer=` param, write audit rows)
- `src/repos/trust_issuer_repo.rs` — **modify** (add `find_by_id`, `find_by_issuer_url`, `delete`)
- `src/auth.rs` — **modify** (remove inline claim-mapping; call `ClaimMapper`)
- `Cargo.toml` — **modify** (add `openidconnect = "3"`)

**Code shapes:**

```sql
-- 0020_trust_issuers_oidc.sql
ALTER TABLE trust_issuers
    ADD COLUMN idp_type                TEXT    NOT NULL DEFAULT 'oidc',
    ADD COLUMN max_coc_level           INTEGER NOT NULL DEFAULT 100,
    ADD COLUMN client_id               TEXT,
    ADD COLUMN client_secret_ciphertext BYTEA,
    ADD COLUMN display_name            TEXT;   -- shown on public /auth/issuers picker
-- jwks_uri already exists, do NOT re-add

-- federated_identities — binds (trust_issuer, OIDC sub claim) → local user.
-- Email is profile data; the immutable subject is the identity. (Codex finding #5.)
CREATE TABLE federated_identities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer_id       UUID NOT NULL REFERENCES trust_issuers(id) ON DELETE RESTRICT,
    subject         TEXT NOT NULL,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_seen_email TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issuer_id, subject),
    UNIQUE (user_id, issuer_id)
);
CREATE INDEX federated_identities_user ON federated_identities(user_id);
-- No RLS — federated_identities is reached from the auth path before org context is set.
-- Safety comes from the secrecy of the (issuer_id, subject) tuple, validated by ID-token signature.
```

```rust
// src/services/idp_service.rs
pub struct IdpService<'a> { pool: &'a PgPool, http: &'a reqwest::Client }

impl<'a> IdpService<'a> {
    pub async fn authorize_url(&self, ti: &TrustIssuer, redirect_uri: &str)
        -> anyhow::Result<(String /* url */, String /* nonce */, String /* pkce_verifier */)>;

    pub async fn exchange_code(&self, ti: &TrustIssuer, code: &str, pkce_verifier: &str,
        expected_nonce: &str, redirect_uri: &str)
        -> anyhow::Result<openidconnect::IdTokenClaims<...>>;
}

// Validates: iss matches ti.issuer_url, aud contains ti.client_id, nonce matches expected_nonce,
// exp > now, iat within ±5min skew. Uses openidconnect::CoreClient.

// src/services/claim_mapper.rs
pub struct ClaimMapper;
impl ClaimMapper {
    pub async fn map_to_user(pool: &PgPool, org_id: Uuid, ti: &TrustIssuer, claims: &Value)
        -> anyhow::Result<User>;
}
// 1. Reads required `sub` claim from ID token (always present in OIDC).
// 2. Looks up federated_identities by (ti.id, sub). If found → fetch users row; update last_seen_email
//    and updated_at. This is the steady-state path.
// 3. If not found → create new users row, insert federated_identities binding. Email comes from
//    ti.claim_mapping.email_claim and is profile data — NOT used for identity lookup.
// 4. Reads role_claim and coc_level_claim, caps coc_level at ti.max_coc_level, upserts roles row,
//    binds user via memberships.
// 5. Returns the User. Email-collision case: two distinct subjects with the same email map to two
//    distinct users (correct behavior — email is mutable, subject is not).
```

**Entra-ID-specific defaults documented in the operator setup guide:**
- `issuer_url` = `https://login.microsoftonline.com/{tenant_id}/v2.0`
- `jwks_uri` = `https://login.microsoftonline.com/{tenant_id}/discovery/v2.0/keys`
- `audience` (`client_id`) = the app registration ID
- `claim_mapping.email_claim` = `"preferred_username"` or `"email"`
- `claim_mapping.role_claim` = `"roles"` (Entra app-role claim)

**Gate:**
```bash
cargo clippy --all-targets -- -D warnings && cargo test --test phase_oidc_callback -- --ignored --test-threads=1
```

**Acceptance:**
- New integration test `phase_oidc_callback` brings up a mock OIDC issuer (using `openidconnect`'s test fixtures) and asserts `/auth/login → /auth/callback` round-trip creates a `user_sessions` row with `idp_type='oidc'`, a `federated_identities` row binding (issuer_id, sub) to the new user, and an `identity_audit_events` row with `event_type='oidc_login'`, `outcome='success'`.
- Second login with the same `sub` reuses the existing user row (no duplicate users created) and updates `federated_identities.last_seen_email` if the email claim changed.
- Two distinct `sub` values asserting the same email claim produce two distinct user rows (email is not the identity).
- Bad `aud` claim → 400, audit row with `outcome='failure'`, `detail.failure_reason='aud_mismatch'`.
- Claim asserting `coc_level=999` against `max_coc_level=50` → resulting `roles.coc_level` is 50.

---

### Phase 3 — Trust issuer admin (S4)

**Goal:** operators can register/list/delete IdPs without psql.

**Files:**
- `src/routes/admin/mod.rs` — **create**
- `src/routes/admin/trust_issuers.rs` — **create**
- `src/routes/public_issuers.rs` — **create** (public discovery endpoint for the login picker)
- `src/routes/mod.rs` — **modify** (mount `/api/v1/admin/*` under auth + admin-role guard; mount `/auth/issuers` public)
- `static/admin.html` — **create** (admin shell page)
- `static/admin.js` — **create** (trust issuer CRUD UI)
- `static/admin.css` — **create** (admin-section styles; loaded from admin.html so we don't append to the shared style.css from a parallel task)

**Code shapes:**

```rust
// src/routes/admin/trust_issuers.rs
#[derive(Deserialize)]
pub struct CreateTrustIssuerBody {
    pub idp_type: String,                  // server enforces == "oidc" in v0.1
    pub issuer_url: String,                // https-only; max 512
    pub audience: String,                  // client_id; max 256
    pub jwks_uri: String,                  // required for OIDC; max 512
    pub claim_mapping: serde_json::Value,
    pub max_coc_level: i32,                // 0..=100
    pub client_secret: Option<String>,     // versioned-encrypted into client_secret_ciphertext
}

#[derive(Serialize)]
pub struct TrustIssuerResp {
    pub id: Uuid,
    pub idp_type: String,
    pub issuer_url: String,
    pub audience: String,
    pub jwks_uri: String,
    pub max_coc_level: i32,
    pub claim_mapping: serde_json::Value,
    // client_secret_ciphertext NEVER returned
}

pub async fn list(ctx, state) -> Json<Vec<TrustIssuerResp>>;
pub async fn create(ctx, state, Json<CreateTrustIssuerBody>) -> Result<Json<TrustIssuerResp>, AppError>;
// create writes IdentityEvent::TrustIssuerCreate audit row with detail = {issuer_url, audience, idp_type}.
pub async fn delete(ctx, state, Path<Uuid>) -> Result<StatusCode, AppError>;
// delete writes IdentityEvent::TrustIssuerDelete audit row.
```

```rust
// src/routes/public_issuers.rs — UNAUTHENTICATED endpoint for the login picker.
// Returns only safe display metadata; no claim_mapping, no client_id, no client_secret, no jwks_uri.
#[derive(Serialize)]
pub struct PublicIssuerResp {
    pub id: Uuid,
    pub display_name: String,   // operator-set human label; falls back to issuer_url host
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<PublicIssuerResp>> { ... }
// Route: GET /auth/issuers
// Mounted on the public router subtree (NOT under enforce_auth).
```

```html
<!-- static/admin.html: simple form with IdP picker (Entra ID / Login.gov / Custom OIDC) -->
<!-- Custom OIDC reveals raw fields. Presets fill issuer_url, jwks_uri, claim_mapping. -->
```

**Admin role check:** `AuthContext.active_role_id` resolves to a `roles` row with `coc_level >= 80`. Single helper in `src/auth.rs`: `pub fn require_admin(ctx: &AuthContext, pool: &PgPool) -> impl Future<...>`.

**Gate:**
```bash
cargo clippy --all-targets -- -D warnings && cargo test trust_issuer_admin -- --nocapture
```

**Acceptance:**
- `POST /api/v1/admin/trust-issuers` with valid Entra ID body returns 200, inserts a row, and writes an `identity_audit_events` row with `event_type='trust_issuer_create'`.
- Same call without admin-level session → 403.
- POST with `idp_type: "saml"` → 400.
- Duplicate `(org_id, issuer_url, audience)` → 409.
- `DELETE /api/v1/admin/trust-issuers/:id` writes an `identity_audit_events` row with `event_type='trust_issuer_delete'`.
- `GET /auth/issuers` (no auth cookie) returns 200 with `[{id, display_name}]` only — body must not contain any of `claim_mapping`, `client_id`, `client_secret_ciphertext`, `jwks_uri`, `audience`. Test asserts each forbidden key is absent.
- Browser smoke: navigate to `/admin.html`, see the IdP picker, submit "Entra ID" preset with tenant_id + client_id, see the new IdP appear in the list.

---

### Phase 4 — TOTP MFA (S3)

**Goal:** TOTP enrollment, challenge, recovery codes — fully wired through the session row's `mfa_verified` flag.

**Files:**
- `migrations/00000000000021_mfa.sql` — **create**
- `src/repos/mfa_repo.rs` — **create**
- `src/services/mfa_service.rs` — **create**
- `src/routes/mfa.rs` — **create**
- `src/routes/mod.rs` — **modify** (mount `/api/v1/me/mfa/*`)
- `src/error.rs` — **modify** (add `AppError::MfaRequired → 403 + body {"error": "mfa_required"}`)
- `static/mfa.html` — **create** (enroll + challenge UI + recovery-codes view)
- `static/mfa.js` — **create**
- `static/mfa.css` — **create** (loaded from mfa.html; avoids style.css collision with admin task)
- `Cargo.toml` — **modify** (add `totp-lite`, `argon2`, `qrcode`, `data-encoding`)

**Code shapes:**

```sql
-- 0021_mfa.sql
CREATE TABLE mfa_enrollments (
    id                         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                    UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    org_id                     UUID NOT NULL REFERENCES organizations(id),
    totp_secret_ciphertext     BYTEA NOT NULL,
    activated_at               TIMESTAMPTZ,
    recovery_codes_viewed_at   TIMESTAMPTZ,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE mfa_enrollments ENABLE ROW LEVEL SECURITY;
CREATE POLICY mfa_enrollments_org_isolation ON mfa_enrollments
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

CREATE TABLE mfa_recovery_codes (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id     UUID NOT NULL REFERENCES organizations(id),
    code_hash  TEXT NOT NULL,            -- argon2id
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX mfa_recovery_unused ON mfa_recovery_codes(user_id) WHERE used_at IS NULL;
ALTER TABLE mfa_recovery_codes ENABLE ROW LEVEL SECURITY;
CREATE POLICY mfa_recovery_org_isolation ON mfa_recovery_codes
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

ALTER TABLE organizations ADD COLUMN mfa_required_for_admins BOOLEAN NOT NULL DEFAULT false;
```

```rust
// src/services/mfa_service.rs
pub struct MfaService<'a> { pool: &'a PgPool, audit: &'a IdentityAuditWriter<'a> }

impl<'a> MfaService<'a> {
    pub async fn enroll_totp(&self, user_id: Uuid, org_id: Uuid, account_label: &str)
        -> anyhow::Result<(String /* otpauth_uri */, String /* secret_b32 */)>;
    pub async fn confirm_totp(&self, user_id: Uuid, code: &str) -> anyhow::Result<()>;
    pub async fn verify(&self, user_id: Uuid, code_or_recovery: &str) -> anyhow::Result<bool>;
    pub async fn issue_recovery_codes(&self, user_id: Uuid) -> anyhow::Result<Vec<String>>;
    pub async fn delete_totp(&self, user_id: Uuid, current_code: &str) -> anyhow::Result<()>;
    pub async fn status(&self, user_id: Uuid) -> anyhow::Result<MfaStatus>;
}

// 30-second TOTP step, ±1 step skew tolerance. Secret = 20 random bytes → 32-char base32.
// Recovery codes: 8 codes, 16 random chars each (base32), argon2id-hashed at insert.
```

**Routes:**
- `GET /api/v1/me/mfa` → `MfaStatus { totp_enrolled, recovery_codes_remaining }`
- `POST /api/v1/me/mfa/totp/enroll`, `/confirm`, `DELETE /totp`
- `POST /api/v1/me/mfa/challenge` → on success, calls `SessionService::mark_mfa_verified`
- `GET /api/v1/me/mfa/recovery-codes`, `POST /recovery-codes/consume`

**Policy enforcement (revised after Codex review):** two-axis check via `mfa_gate(ctx, pool)` helper, invoked from broker endpoints AND every admin route:

| Org `mfa_required_for_admins` | User has admin role | User has enrollment | `mfa_verified` on session | Result |
|---|---|---|---|---|
| `true` | yes | no | — | 403 `{"error": "mfa_enrollment_required"}` — UI redirects to `/mfa/enroll` |
| `true` | yes | yes | false | 403 `{"error": "mfa_required"}` — UI redirects to `/mfa/challenge` |
| `true` | yes | yes | true | proceed |
| `true` | no | yes | false | 403 `{"error": "mfa_required"}` |
| `true` | no | yes | true | proceed |
| `true` | no | no | — | proceed (non-admin opt-in, not forced) |
| `false` | any | yes | false | 403 `{"error": "mfa_required"}` |
| `false` | any | yes | true | proceed |
| `false` | any | no | — | proceed |

The `mfa_enrollment_required` error is distinct from `mfa_required` so the UI knows whether to push the user through enrollment or challenge. Admin-with-no-enrollment cannot bypass MFA — they are blocked at every admin endpoint until they enroll. (Closes Codex finding #6.)

**Gate:**
```bash
cargo clippy --all-targets -- -D warnings && cargo test mfa -- --nocapture
```

**Acceptance:**
- Enroll → confirm with correct TOTP → 204, session's `mfa_verified` flips to `true`.
- Challenge with wrong code → 403, audit row `event_type='mfa_fail'`.
- GET recovery-codes twice → second call 409.
- Wrong code 5 times → still 403 each time (no rate limit in v0.1, but 5 audit rows exist).
- Delete TOTP without supplying current code → 400. Successful TOTP delete writes `identity_audit_events` row with `event_type='mfa_disable'`.
- Admin user with no MFA enrollment, org flag `mfa_required_for_admins=true`, hits `GET /api/v1/admin/trust-issuers` → 403 with body `{"error": "mfa_enrollment_required"}`. After enrollment + challenge, the same call succeeds.
- Recovery code consumption writes `identity_audit_events` row with `event_type='mfa_recovery_consume'`.

---

### Phase 5 — Brokered SaaS OAuth (S5, generic flow + schema)

**Goal:** IONe holds delegated OAuth tokens per `(user, provider)` via a generic OAuth flow. Provider-specific adapters (QuickBooks, Google) defer to v0.2.

**Files:**
- `migrations/00000000000022_broker_credentials.sql` — **create**
- `src/util/token_crypto.rs` — **modify** (add `encrypt_versioned` / `decrypt_versioned`)
- `src/repos/broker_credential_repo.rs` — **create**
- `src/services/brokered_token_service.rs` — **create**
- `src/routes/broker.rs` — **create**
- `src/routes/mod.rs` — **modify** (mount `/api/v1/broker/*` and `/auth/broker/callback`)
- `static/connections.html` — **create**
- `static/connections.js` — **create**
- `config/broker_providers.toml` — **create** (provider registry: name, authorize_url, token_url, revoke_url, scopes_required)

**Code shapes:**

```sql
-- 0022_broker_credentials.sql
CREATE TABLE broker_credentials (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id                   UUID NOT NULL REFERENCES organizations(id),
    provider                 TEXT NOT NULL,
    label                    TEXT NOT NULL DEFAULT '',   -- empty string acts as default label; allows multiple accounts per provider
    scopes                   TEXT[] NOT NULL DEFAULT '{}',
    access_token_ciphertext  BYTEA,
    refresh_token_ciphertext BYTEA,
    token_expires_at         TIMESTAMPTZ,
    state_token              TEXT,
    code_verifier            TEXT,
    state_expires_at         TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Multiple accounts per provider per user are valid (different QuickBooks companies, different Google accounts).
    -- label disambiguates. (Closes Codex finding #8.)
    UNIQUE (user_id, provider, label)
);
CREATE INDEX broker_credentials_expiring ON broker_credentials(token_expires_at)
    WHERE access_token_ciphertext IS NOT NULL;
-- state_token uniqueness — required for the public callback to look up the pending row safely.
CREATE UNIQUE INDEX broker_state_token_unique
    ON broker_credentials(state_token) WHERE state_token IS NOT NULL;

ALTER TABLE broker_credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY broker_credentials_org_isolation ON broker_credentials
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
```

```rust
// src/util/token_crypto.rs — additions
pub const TOKEN_KEY_VERSION_CURRENT: u8 = 0x01;

pub fn encrypt_versioned(plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    // layout: [1B version][12B nonce][ciphertext+tag]
}

pub fn decrypt_versioned(ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    // reads version byte, picks key, decrypts. Returns Err(DecryptionError) on key mismatch.
}

// src/services/brokered_token_service.rs
pub struct BrokeredTokenService<'a> { ... }

impl<'a> BrokeredTokenService<'a> {
    pub async fn begin_connection(&self, user_id, org_id, provider: &str, scopes: &[String], label: Option<String>)
        -> anyhow::Result<(Uuid /* connection_id */, String /* authorize_url */)>;

    pub async fn complete_callback(&self, state_token: &str, code: &str)
        -> anyhow::Result<()>;

    pub async fn get_for_user(&self, user_id: Uuid, provider: &str)
        -> anyhow::Result<Option<String /* plaintext access_token */>>;
    // refreshes if token_expires_at < now + 5min

    pub async fn revoke(&self, user_id: Uuid, connection_id: Uuid) -> anyhow::Result<()>;
}
```

```toml
# config/broker_providers.toml
[providers.generic-test]
authorize_url = "${IONE_TEST_AUTHORIZE_URL}"
token_url     = "${IONE_TEST_TOKEN_URL}"
revoke_url    = "${IONE_TEST_REVOKE_URL}"
scopes_required = []
# Future: [providers.quickbooks] / [providers.google_workspace] populated in v0.2
```

**Routes:**
- `POST /api/v1/broker/connections` → `{ connection_id, authorize_url }`
- `GET /api/v1/broker/connections` → list
- `DELETE /api/v1/broker/connections/:id` → revoke (best-effort upstream)
- `GET /auth/broker/callback?code=&state=&error=` → public, completes exchange
- `POST /api/v1/broker/connections/:id/refresh`

**Gate:**
```bash
cargo clippy --all-targets -- -D warnings && cargo test broker -- --ignored --test-threads=1
```

**Acceptance:**
- Test runs against a local mock OAuth provider (axum sub-app). POST → returns authorize_url. Test client follows to mock IdP, IdP redirects to callback. `broker_credentials` row has `access_token_ciphertext` non-null, `state_token` and `code_verifier` cleared, `token_expires_at` populated. Audit row `event_type='token_broker_grant'` written.
- `BrokerService::get_for_user` against an expired token → triggers refresh via the mock provider, updates ciphertext. Audit row `event_type='token_broker_refresh'` written.
- **Replay test:** same `state_token` POSTed to `/auth/broker/callback` twice — first call 302 success, second call 400 (state_token was cleared after first use).
- **Expired-state test:** `state_token` whose `state_expires_at < now()` → 400 with `error: "broker_state_expired"`.
- **Multi-account test:** same user connects two QuickBooks accounts (provider="quickbooks", different `label` values: `"Acme"`, `"Beta Corp"`). Both rows insert successfully; `UNIQUE(user_id, provider, label)` rejects only an exact triplet collision.
- Ciphertext with wrong key-version byte → `decrypt_versioned` returns `Err(DecryptionError)` and does not panic.
- Upstream revocation HTTP call fails on DELETE → local row still deleted; audit row written with `event_type='token_broker_revoke_upstream_failed'`, `outcome='failure'`. A second DELETE row with `event_type='token_broker_revoke'`, `outcome='success'` follows.
- Browser smoke: `/connections.html` shows the generic-test provider, clicking "Connect" completes the round-trip to the mock provider.

---

### Phase 6 — Org-scoped peers (S7)

**Goal:** add `org_id` to `peers` so cross-app workspace context (out of this design) can land cleanly later, and so `workspace_peer_bindings` (substrate layer 6) has a foreign key path.

**Files:**
- `migrations/00000000000023_peers_org_id.sql` — **create**
- `src/routes/peers.rs` — **modify** (filter every query by `ctx.org_id`)
- `src/services/peer_oauth.rs` — **modify** (carry org_id through pending/complete)

**Code shapes:**

```sql
-- 0023_peers_org_id.sql
ALTER TABLE peers ADD COLUMN org_id UUID REFERENCES organizations(id);
UPDATE peers SET org_id = (SELECT id FROM organizations ORDER BY created_at LIMIT 1) WHERE org_id IS NULL;
ALTER TABLE peers ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX peers_org ON peers(org_id);
```

**Gate:**
```bash
cargo sqlx migrate run && cargo clippy --all-targets -- -D warnings && cargo test peers -- --nocapture
```

**Acceptance:**
- New integration test: two orgs, one peer per org, list-peers as org-A operator returns only org-A's peer rows.
- All existing peer integration tests still pass after the migration.

---

### Phase 7 — Login UI polish + IdP picker + MFA challenge interstitial

**Goal:** the operator-facing login experience for v0.1. Small phase; ties UI to the new backend.

**Files:**
- `static/login.html` — **create** (or modify if exists)
- `static/login.js` — **create**
- `static/app.js` — **modify** (on 403 `mfa_required` body, redirect to `/mfa/challenge.html`)
- `static/style.css` — **modify**

**Code shapes:** vanilla JS — IdP picker reads `GET /api/v1/admin/trust-issuers` (or a public, redacted variant), shows one button per IdP, redirects to `/auth/login?issuer=<url>`. If only one IdP is registered, auto-redirect.

**Gate:**
```bash
# Manual browser smoke; no automated UI gate in this stack.
cargo run --release & sleep 3 && curl -sSI http://localhost:3002/login.html | head -1
```

**Acceptance:**
- With one IdP registered (Entra ID test tenant), navigating to `/login.html` redirects immediately to `/auth/login?issuer=<entra>`.
- With two IdPs registered, the picker renders both.
- After login, hitting any `mfa_verified`-gated route returns 403 with body `{"error": "mfa_required"}` and the SPA redirects to MFA challenge.

---

## Phase summary (file counts)

| Phase | New files | Modified | Migrations |
|---|---|---|---|
| 0 — Prerequisites | 1 SQL | 3 Rust | 1 |
| 1 — Sessions + audit | 3 Rust | 3 Rust | 1 |
| 2 — OIDC consumer | 2 Rust | 3 Rust + Cargo.toml | 1 |
| 3 — Trust issuer admin | 4 (2 Rust + 2 static) | 2 (Rust + CSS) | 0 |
| 4 — TOTP MFA | 5 (3 Rust + 2 static) | 3 (Rust + Cargo.toml + CSS) | 1 |
| 5 — Brokered SaaS OAuth | 6 (4 Rust + 2 static + 1 TOML) | 2 (Rust) | 1 |
| 6 — Org-scoped peers | 0 | 2 Rust | 1 |
| 7 — Login UI | 2 static | 2 static + CSS | 0 |
| **Total** | **~25** | **~22** | **6** |

## v0.1 known limitations (documented, not blockers)

Codex review surfaced abuse-resistance gaps that are out of scope for v0.1 but documented here so operators and v0.2 planners can act on them:

1. **No rate limiting on authentication endpoints.** OIDC callback errors, MFA challenges, MFA recovery code consumption, and broker callback attempts can be retried arbitrarily often. Mitigation in v0.1: every attempt writes an `identity_audit_events` row with `outcome='failure'`, so operators can detect abuse via SQL or log queries against the audit table. v0.2 will add per-IP throttling (likely via `tower-governor` or equivalent) and an account-lockout decision for repeated MFA failure.

2. **No proactive token refresh scheduler for broker credentials.** Brokered SaaS tokens refresh on read when `token_expires_at < now() + 5min`. This means a token can be invalid for a brief window if no read happens before expiry and the upstream provider does not honor expired-but-refreshable semantics. Operators with active QuickBooks/Google use cases should re-trigger a `BrokerService::get_for_user` call after long idle periods. v0.2 will add a background scheduler that refreshes within 24h of expiry.

3. **RLS context is per-request, not per-connection-pool.** Every protected request opens a transaction, sets `app.current_org_id`, and commits at response time. This adds one round-trip per request. At small scale (operators measured in tens) the cost is negligible; at scale, `tokio_postgres` connection-level context or PgBouncer session-level context may be preferred. v0.2 will revisit if observability shows latency impact.

4. **No federated logout (SLO).** `POST /auth/logout` revokes the IONe session and clears the cookie but does not propagate to Entra ID / Login.gov / upstream IdP. The operator's upstream session continues until that IdP expires it. Documented in the operator deployment guide; v0.2 may add OIDC back-channel logout for Entra-ID-integrated deployments.

5. **Single-tenant by default.** One org per IONe deployment. Multi-tenant identity (multiple buyer tenants with distinct IdPs in one IONe instance) is v0.2+ and depends on the v0.2 RLS retrofit of older tables.

6. **RLS policies are present but not yet enforced.** Migrations 0019, 0021, and 0022 add `ENABLE ROW LEVEL SECURITY` + `CREATE POLICY` clauses on `user_sessions`, `identity_audit_events`, `mfa_enrollments`, `mfa_recovery_codes`, and `broker_credentials`. The policies are intentionally inert in v0.1: the IONe Postgres role owns these tables, so PostgreSQL bypasses RLS for the owner unless `FORCE ROW LEVEL SECURITY` is set. Application-layer scoping (every query filters by `ctx.org_id` / `ctx.user_id`) provides the actual isolation today. The policies are defense-in-depth scaffolding for the v0.2 work that will (a) introduce a non-owner DB role for application connections, (b) add a per-request transaction middleware that calls `set_config('app.current_org_id', ctx.org_id, true)`, and (c) add `FORCE ROW LEVEL SECURITY` to flip the policies from inert to enforced. Do not assume the policies are doing isolation work in v0.1.

## Task Manifest

Routing: `claude-code` for tasks touching existing code with multiple callers or middleware integration; `codex` for greenfield service modules and static HTML/JS from clear specs.

| Task | Agent | Files | Depends On | Gate |
|------|-------|-------|------------|------|
| T0a — P-1: peer OAuth pending DB-backed nonce | claude-code | `migrations/00000000000018_peer_oauth_pending.sql`, `src/routes/peers.rs` | — | `cargo test --test phase11_peer -- --ignored --test-threads=1` |
| T0b — P-2: enforce_auth + 401 in OIDC mode | claude-code | `src/auth.rs`, `src/routes/mod.rs` | — | `curl -o /dev/null -w "%{http_code}" /api/v1/workspaces` returns 401 unauth |
| T0c — P-3: CORS allowlist | claude-code | `src/routes/mod.rs` | T0b (shares `src/routes/mod.rs`) | Bad-Origin curl does not echo Access-Control-Allow-Origin |
| T1 — Sessions + audit-event infra | claude-code | `migrations/00000000000019_user_sessions_audit.sql`, `src/repos/user_session_repo.rs`, `src/services/session_service.rs`, `src/services/identity_audit_writer.rs`, `src/auth.rs`, `src/repos/mod.rs`, `src/services/mod.rs`, `src/models/mod.rs` | T0b | `cargo test session -- --nocapture` |
| T2 — OIDC consumer w/ Entra ID defaults | claude-code | `migrations/00000000000020_trust_issuers_oidc.sql`, `src/services/idp_service.rs`, `src/services/claim_mapper.rs`, `src/routes/auth_routes.rs`, `src/repos/trust_issuer_repo.rs`, `src/auth.rs`, `Cargo.toml` | T1 | `cargo test --test phase_oidc_callback -- --ignored --test-threads=1` |
| T3a — Trust issuer admin API | codex | `src/routes/admin/mod.rs`, `src/routes/admin/trust_issuers.rs`, `src/routes/mod.rs` | T2 | `cargo test trust_issuer_admin -- --nocapture` |
| T3b — Trust issuer admin UI | codex | `static/admin.html`, `static/admin.js`, `static/admin.css` | T3a | manual: navigate to /admin.html, submit Entra preset, see new IdP listed |
| T4a — TOTP MFA backend | codex | `migrations/00000000000021_mfa.sql`, `src/repos/mfa_repo.rs`, `src/services/mfa_service.rs`, `src/routes/mfa.rs`, `src/routes/mod.rs`, `src/error.rs`, `Cargo.toml` | T1 | `cargo test mfa -- --nocapture` |
| T4b — MFA UI | codex | `static/mfa.html`, `static/mfa.js`, `static/mfa.css` | T4a | manual: enroll TOTP, scan QR in authenticator, confirm code, see recovery codes once |
| T5a — Broker token crypto + service | claude-code | `src/util/token_crypto.rs`, `migrations/00000000000022_broker_credentials.sql`, `src/repos/broker_credential_repo.rs`, `src/services/brokered_token_service.rs` | T1 | `cargo test token_crypto_versioned -- --nocapture` |
| T5b — Broker routes + provider registry | claude-code | `src/routes/broker.rs`, `src/routes/mod.rs`, `config/broker_providers.toml` | T5a | `cargo test broker -- --ignored --test-threads=1` |
| T5c — Broker UI | codex | `static/connections.html`, `static/connections.js` | T5b | manual: connect generic-test provider, see active connection |
| T6 — Org-scoped peers | claude-code | `migrations/00000000000023_peers_org_id.sql`, `src/routes/peers.rs`, `src/services/peer_oauth.rs` | T0a | `cargo test peers -- --nocapture` |
| T7 — Login UI + MFA interstitial | codex | `static/login.html`, `static/login.js`, `static/app.js`, `static/login.css` | T2, T4b | manual: one-IdP and two-IdP login flows; mfa_required redirect works |

**Parallel groups:**
- **Group A (Phase 0):** T0a, T0b, T0c — disjoint file sets, run in parallel.
- **Group B (after T1, T2):** T3a, T4a, T5a — disjoint file sets, run in parallel.
- **Group C (after Group B):** T3b, T4b, T5b — disjoint file sets (UI for admin; MFA UI; broker routes), run in parallel.
- **T6** can run any time after T0a; defer to end if convenient.
- **T7** runs last; depends on T2 and T4b.

## Self-review

1. **Every design AC mapped to a phase gate?** AC-1, AC-2, AC-3 → Phase 1 gate. AC-4, AC-5, AC-6 → Phase 2 gate. AC-7, AC-8, AC-9 → Phase 4 gate. AC-10 → Phase 3 gate. AC-11, AC-12 → Phase 5 gate. AC-13 → Phase 1 (audit infra) + per-phase verification. AC-14 → Phase 6 gate. AC-15 → Phase 1 + Phase 4 + Phase 5 (RLS policies created at each table). **Yes.**
2. **Every cited file exists now or is in the file inventory?** Existing files cited: `src/auth.rs`, `src/routes/peers.rs`, `src/routes/mod.rs`, `src/routes/auth_routes.rs`, `src/util/token_crypto.rs`, `src/services/peer_oauth.rs`, `src/repos/trust_issuer_repo.rs`, `src/services/mod.rs`, `src/repos/mod.rs`, `src/models/mod.rs`, `src/error.rs`, `static/app.js`, `static/style.css`, `Cargo.toml`. All confirmed at HEAD. New files are listed under each phase's "Files" with **create** markers. **Yes.**
3. **Phases are vertical slices?** Each phase ships one feature DB+API+UI together. The closest exception is Phase 1 which is foundational (no UI) — but it ships the audit infra used by every later phase, so it's a substrate slice, not a layer stack. Phases 2 through 7 are clean vertical slices. **Yes.**
4. **Gates are concrete shell commands?** Every gate names an explicit `cargo` or `curl` command with arguments. **Yes.**
5. **Parallel tasks have disjoint file sets?** Group A (T0a/T0b/T0c): T0b and T0c both touch `src/routes/mod.rs` → **NOT disjoint.** Fix: sequence T0c after T0b. Group B (T3a/T4a/T5a): admin/mod.rs vs mfa.rs vs token_crypto.rs+broker — disjoint. Group C (T3b/T4b/T5b): admin.html/admin.js/style.css vs mfa.html/mfa.js/style.css vs broker.rs/mod.rs — both T3b and T4b touch `static/style.css` → **NOT disjoint.** Fix: sequence T4b after T3b for style.css, or split style additions into separate files (`admin.css`, `mfa.css`) loaded from each HTML page.

**Self-review fixes applied:**
- **T0c sequenced after T0b** (both touch `src/routes/mod.rs`). Updated parallel-groups note below.
- **T3b and T4b**: each creates its own CSS file (`static/admin.css` for T3b, `static/mfa.css` for T4b) rather than appending to `style.css`. `static/style.css` removed from both task file lists; new CSS files added to each.

Revised parallel groups:
- **Group A (Phase 0):** T0a and T0b in parallel; T0c after T0b.
- **Group B:** T3a, T4a, T5a in parallel after T2.
- **Group C:** T3b, T4b, T5b in parallel after Group B.
