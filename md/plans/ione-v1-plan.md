# IONe v1 — Implementation Plan

**Source design:** [md/design/ione-v1.md](../design/ione-v1.md)
**Contract:** [md/design/ione-v1-contract.md](../design/ione-v1-contract.md)
**Team:** 2 analyst-programmers
**Stack:** Rust (axum, tokio, reqwest, tower-http, sqlx) · Postgres 16 + pgvector · MinIO/S3 · Ollama · vanilla HTML/JS served by the Rust binary · Apache 2.0 · docker-compose for local dev

## Status snapshot (2026-04-22)

This plan is now a record of the shipped `v0.1.0` implementation, not an active task queue. Phases 1-14 landed on `main`, the OSS release was cut on 2026-04-20, and the task manifest below should be read as completed delivery history.

Local runtime validation on 2026-04-22:
- `cargo run --release` booted successfully against local Postgres + MinIO with runtime migrations applied on startup.
- `GET /api/v1/health` returned `{"status":"ok","version":"0.1.0"}`.
- Local-auth `GET /api/v1/me` returned the default bootstrap user and membership.
- A smoke workspace + conversation successfully round-tripped a live Ollama reply (`Pong.`) through `/api/v1/conversations/:id/messages`.

Immediate follow-on work should live in a new post-`v0.1.0` plan rather than reopening this document.

## Phasing principle

Vertical slices only — each phase lands DB + API + UI (where applicable) + integration test for one feature, and ends with a working system a human can try. No layer-only phases.

Each phase has:
- **Goal** (one sentence)
- **Dependencies added**
- **Files changed**
- **Data structures** (SQL / Rust / JS) — full definitions, not prose
- **Red/Green/Refactor cycle** — failing test first, minimal implementation, clean-up
- **Wiring checklist** — grep-able mechanical assertions
- **Exit criteria** — commands that must pass
- **Risk callouts and non-goals**

## Repository layout (established in Phase 1)

```
ione/
├── Cargo.toml              # single crate; workspace added in Phase 9 if needed
├── docker-compose.yml      # Phase 2+
├── .env.example
├── migrations/             # sqlx migrations; added Phase 2
│   └── 0001_initial.sql
├── src/
│   ├── main.rs             # bootstrap, config, router wiring
│   ├── config.rs
│   ├── error.rs            # thiserror -> axum IntoResponse
│   ├── state.rs            # AppState (db pool, http client, config)
│   ├── routes/
│   │   ├── mod.rs
│   │   ├── health.rs
│   │   ├── chat.rs
│   │   ├── conversations.rs
│   │   ├── workspaces.rs
│   │   ├── connectors.rs
│   │   ├── signals.rs
│   │   ├── survivors.rs
│   │   ├── artifacts.rs
│   │   ├── approvals.rs
│   │   ├── peers.rs
│   │   └── auth.rs
│   ├── services/
│   │   ├── ollama.rs       # chat + generate
│   │   ├── embeddings.rs
│   │   ├── rules.rs
│   │   ├── generator.rs
│   │   ├── critic.rs
│   │   ├── router.rs       # routing classifier
│   │   └── approvals.rs
│   ├── connectors/
│   │   ├── mod.rs          # Connector trait
│   │   ├── openapi.rs      # OpenAPI auto-adapter
│   │   ├── mcp_client.rs   # outgoing MCP
│   │   ├── nws.rs          # hand-wired fallback / seed config
│   │   ├── firms.rs
│   │   ├── slack.rs
│   │   ├── smtp.rs
│   │   ├── fs_s3.rs
│   │   └── irwin.rs
│   ├── mcp_server.rs       # IONe-as-MCP
│   ├── auth.rs             # OIDC middleware
│   └── audit.rs
├── static/
│   ├── index.html
│   ├── app.js
│   └── style.css
├── tests/
│   ├── phase01_chat.rs
│   ├── phase02_conversations.rs
│   └── …
└── README.md
```

## Phase 1 — Chat ping→pong (Rust ↔ Ollama)

**Goal:** type a message in the browser, get a model reply back from local Ollama, end-to-end. No DB, no auth, no workspaces.

**Dependencies (Cargo.toml):**
```toml
axum = "0.7"
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.5", features = ["fs", "trace", "cors"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
thiserror = "1"
```

**Files created:** `src/main.rs`, `src/config.rs`, `src/error.rs`, `src/state.rs`, `src/routes/{mod.rs,health.rs,chat.rs}`, `src/services/ollama.rs`, `static/{index.html,app.js,style.css}`, `tests/phase01_chat.rs`, `README.md`, `LICENSE` (Apache 2.0).

**Rust shapes:**
```rust
// src/routes/chat.rs
#[derive(Deserialize)]
pub struct ChatRequest { pub model: Option<String>, pub prompt: String }

#[derive(Serialize)]
pub struct ChatResponse { pub reply: String, pub model: String }

// src/services/ollama.rs
pub struct OllamaClient { base_url: String, http: reqwest::Client }
impl OllamaClient {
    pub async fn chat(&self, model: &str, prompt: &str) -> anyhow::Result<String> { /* POST /api/generate stream=false */ }
}
```

**Config (env):** `IONE_BIND=0.0.0.0:3000`, `OLLAMA_BASE_URL=http://localhost:11434`, `OLLAMA_MODEL=llama3.2:latest`.

**Red:** `tests/phase01_chat.rs` — boot the server on a random port, POST `/api/v1/chat` with `{"prompt":"say pong"}`, expect 200 with non-empty `reply`. Test is `#[ignore]`-gated on `OLLAMA_BASE_URL` because Ollama must be running.

**Green:** implement `/api/v1/chat` → `OllamaClient::chat` → return reply. Serve `static/` under `/`. `/api/v1/health` returns `{"status":"ok","version":…}`.

**Refactor:** factor `OllamaClient` behind `AppState`, add request-id/tracing middleware.

**UI (static/index.html + app.js):** single-page — textarea, send button, scrolling transcript. `fetch('/api/v1/chat', {method:'POST', body: JSON.stringify({prompt})})`. No streaming in Phase 1.

**Wiring checklist:**
- [ ] `grep -n "/api/v1/chat" src/routes/mod.rs` → one match
- [ ] `grep -n "/api/v1/health" src/routes/mod.rs` → one match
- [ ] `grep -n "fetch.*api/v1/chat" static/app.js` → one match
- [ ] `ls static/index.html static/app.js static/style.css` → all present
- [ ] `grep -n "Apache" LICENSE` → non-empty

**Exit criteria:**
```
cargo check
cargo clippy -- -D warnings
cargo fmt --check
OLLAMA_BASE_URL=http://localhost:11434 cargo test --test phase01_chat -- --ignored
# manual: open http://localhost:3000, type "say pong", see model reply
```

**Risks:** Ollama model load latency (first call 5–30s). Mitigate: UI shows "loading…" state, 60s timeout on fetch.

**Non-goals (explicit for Phase 1):** streaming, conversation history, auth, DB, multiple conversations, file uploads.

## Phase 2 — Persistent conversations (Postgres + history)

**Goal:** each chat is stored; reloading the page shows prior conversations and their messages. `docker compose up` brings up Postgres + MinIO + Ollama.

**Dependencies:**
```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono", "json"] }
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
dotenvy = "0.15"
```
Dev: `cargo install sqlx-cli --no-default-features --features postgres` (documented in README, not in Cargo.toml).

**Files created:** `docker-compose.yml`, `.env.example`, `migrations/0001_initial.sql`, `src/routes/conversations.rs`, new UI panels in `static/app.js`, `tests/phase02_conversations.rs`.

**docker-compose.yml (core services):** postgres:16 (+ pgvector), minio, ollama, (Keycloak added Phase 8). One volume per service.

**Migration 0001 (Phase 2 scope only):**
```sql
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TYPE message_role AS ENUM ('user','assistant','system');

CREATE TABLE organizations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  email TEXT NOT NULL,
  display_name TEXT NOT NULL,
  oidc_subject TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (org_id, email)
);

CREATE TABLE conversations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID,
  user_id UUID REFERENCES users(id) ON DELETE SET NULL,
  title TEXT NOT NULL DEFAULT 'Untitled',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE messages (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role message_role NOT NULL,
  content TEXT NOT NULL,
  model TEXT,
  tokens_in INT,
  tokens_out INT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX messages_conversation_id_created_at ON messages(conversation_id, created_at);
```

Phase 2 creates a default organization + user (`default@localhost`) via an idempotent bootstrap in `main.rs` so anonymous users can still use the system until Phase 8 auth lands.

**Rust repo (sqlx direct, no ORM):**
```rust
pub struct ConversationRepo { pub pool: PgPool }
impl ConversationRepo {
    pub async fn create(&self, user_id: Uuid, title: &str) -> Result<Conversation>;
    pub async fn list(&self, user_id: Uuid) -> Result<Vec<Conversation>>;
    pub async fn get(&self, id: Uuid) -> Result<Option<Conversation>>;
}
pub struct MessageRepo { pub pool: PgPool }
impl MessageRepo {
    pub async fn append(&self, conv_id: Uuid, role: MessageRole, content: &str, model: Option<&str>) -> Result<Message>;
    pub async fn list(&self, conv_id: Uuid) -> Result<Vec<Message>>;
}
```

**API additions:** `GET /api/v1/conversations`, `POST /api/v1/conversations`, `GET /api/v1/conversations/:id`, `POST /api/v1/conversations/:id/messages`. `POST /api/v1/chat` from Phase 1 is preserved as the stateless "one-shot" path.

**UI additions:** left sidebar with conversation list; clicking loads messages; "New chat" button creates a new conversation; sending a message appends both user and assistant messages to the transcript and writes through.

**Red:** `tests/phase02_conversations.rs` — create a conversation, post two messages, fetch the conversation, assert 2 assistant + 2 user messages (4 total), assert ordering by `created_at`.

**Wiring checklist:**
- [ ] `grep -n "/api/v1/conversations" src/routes/mod.rs` → two matches (index + nested)
- [ ] `grep -rn "ConversationRepo::" src/routes/` → non-empty
- [ ] `grep -rn "MessageRepo::append" src/routes/` → non-empty
- [ ] `sqlx migrate info` → 0001 applied
- [ ] `grep -n "fetch.*conversations" static/app.js` → non-empty

**Exit criteria:**
```
docker compose up -d postgres
sqlx migrate run
cargo test --test phase02_conversations
curl -s localhost:3000/api/v1/conversations | jq '.items | type' → "array"
```

**Risks:** sqlx offline/prepare dance in CI. Mitigate: `SQLX_OFFLINE=true` + `.sqlx` metadata committed from the start.

**Non-goals:** multi-tenant isolation, role enforcement on read, conversation rename/delete UI.

## Phase 3 — Workspaces and roles

**Goal:** conversations and everything else live inside a `Workspace` with at least one role. UI gains a workspace switcher.

**Migration 0002 additions:**
```sql
CREATE TYPE workspace_lifecycle AS ENUM ('continuous','bounded');

CREATE TABLE workspaces (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  parent_id UUID REFERENCES workspaces(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  domain TEXT NOT NULL DEFAULT 'generic',
  lifecycle workspace_lifecycle NOT NULL DEFAULT 'continuous',
  end_condition JSONB,
  metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  closed_at TIMESTAMPTZ
);

CREATE TABLE roles (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  coc_level INT NOT NULL DEFAULT 0,
  permissions JSONB NOT NULL DEFAULT '{}'::jsonb,
  UNIQUE (workspace_id, name)
);

CREATE TABLE memberships (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  role_id UUID NOT NULL REFERENCES roles(id) ON DELETE RESTRICT,
  federated_claim_ref TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (user_id, workspace_id, role_id)
);

ALTER TABLE conversations
  ADD CONSTRAINT conversations_workspace_fk
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE;
```

**API additions:** list/create/get/close workspaces. Conversations become workspace-scoped.

**UI:** top-bar workspace picker; creating a new workspace creates a default `member` role and assigns the current user. Seed script creates an "Operations" workspace on first boot.

**Red:** test creates workspace A and B, creates a conversation in A, asserts listing conversations in B returns empty.

**Wiring checklist:** route registered, repo method used, migration applied, `workspaceId` sent from UI, foreign key verified with `\d conversations`.

**Exit criteria:** `cargo test --test phase03_workspaces`; UI workspace switcher visible; creating a workspace and sending a chat puts the messages in that workspace's conversation.

**Risks:** migration 0002 adds a FK to an existing column — if Phase 2 data exists, it must first be assigned to a workspace. Mitigate: migration assigns any null `workspace_id` to the seeded "Operations" workspace before adding the FK.

**Non-goals:** role-based permission enforcement (Phase 8), federated claims (Phase 8).

## Phase 4 — First connector: NOAA NWS + stream ingest

**Goal:** register an NWS weather connector in a workspace, pull a stream (current conditions for a lat/lon), see events land in the DB, view them in the UI.

**Migration 0003:**
```sql
CREATE TYPE connector_kind AS ENUM ('mcp','openapi','rust_native');
CREATE TYPE connector_status AS ENUM ('active','paused','error');

CREATE TABLE connectors (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  kind connector_kind NOT NULL,
  name TEXT NOT NULL,
  config JSONB NOT NULL,
  status connector_status NOT NULL DEFAULT 'active',
  last_error TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE streams (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  connector_id UUID NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  schema JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (connector_id, name)
);

CREATE TABLE stream_events (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  stream_id UUID NOT NULL REFERENCES streams(id) ON DELETE CASCADE,
  payload JSONB NOT NULL,
  observed_at TIMESTAMPTZ NOT NULL,
  ingested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  embedding vector(768)
);
CREATE INDEX stream_events_stream_observed ON stream_events(stream_id, observed_at DESC);
```

**Connector trait (src/connectors/mod.rs):**
```rust
#[async_trait]
pub trait Connector: Send + Sync {
    fn kind(&self) -> ConnectorKind;
    async fn list_streams(&self) -> Result<Vec<StreamDescriptor>>;
    async fn poll(&self, stream: &str, cursor: Option<serde_json::Value>) -> Result<PollResult>;
    async fn invoke(&self, op: &str, args: serde_json::Value) -> Result<serde_json::Value>; // used later for commands-down
}
pub struct StreamDescriptor { pub name: String, pub schema: serde_json::Value }
pub struct PollResult { pub events: Vec<StreamEventInput>, pub next_cursor: Option<serde_json::Value> }
```

`src/connectors/nws.rs` implements this with hand-coded `reqwest` calls to `api.weather.gov` (no API key required; User-Agent header required). One stream: `observations` keyed by `{lat, lon}` in `config`.

**API additions:** connector CRUD, stream list, poll endpoint. Background poller added in Phase 5; Phase 4 polls on-demand via `POST /api/v1/streams/:id/poll`.

**UI:** in a workspace, a "Connectors" tab lets you add the NWS connector with lat/lon, then a "Streams" tab lets you click "Poll now" and see the resulting rows.

**Red:** test registers an NWS connector, polls, asserts ≥1 `stream_event` with a non-null `payload.temperature` key.

**Wiring checklist:** route registered, connector kind registry has `rust_native:nws`, UI form posts to `/api/v1/workspaces/:id/connectors`.

**Exit criteria:** `cargo test --test phase04_nws_connector`; UI shows ingested observation timestamps.

**Risks:** NWS API rate limits and outages. Mitigate: polite User-Agent, backoff on 5xx, surface `last_error` in UI.

**Non-goals:** MCP connectors (Phase 9), OpenAPI auto-adapter generalization (Phase 9), scheduled polling (Phase 5).

## Phase 5 — Rules floor + generator pass → signals

**Goal:** a scheduled poller pulls streams, a deterministic rule engine and a generator LLM pass both produce `signals`, the UI shows a signal feed per workspace.

**Dependencies:** `tokio-cron-scheduler = "0.11"` (or a simpler `tokio::time::interval` loop — chosen for simplicity unless scheduling diversity is needed).

**Migration 0004:**
```sql
CREATE TYPE signal_source AS ENUM ('rule','connector_event','generator');
CREATE TYPE severity AS ENUM ('routine','flagged','command');

CREATE TABLE signals (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  source signal_source NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  evidence JSONB NOT NULL DEFAULT '[]'::jsonb,
  severity severity NOT NULL DEFAULT 'routine',
  generator_model TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX signals_workspace_created ON signals(workspace_id, created_at DESC);
```

**Rules (src/services/rules.rs):** each workspace declares rules as JSONB on `workspace.metadata.rules`; a rule is `{ stream: "observations", when: "payload.humidity < 15", severity: "flagged", title: "Humidity critically low" }`. Evaluated in Rust with a small expression evaluator (`evalexpr = "11"` or `jsonpath_lib` — pick `evalexpr`).

**Generator (src/services/generator.rs):** batches recent events per stream, builds a prompt ("summarize notable developments in the last hour that a duty officer should see. Emit JSON matching this schema: { title, body, severity, evidence_event_ids[] }"), calls Ollama with `OLLAMA_GENERATOR_MODEL` (default `qwen3:14b`). Parsed output becomes a `signal` with `source='generator'`.

**Scheduler:** a tokio task loops every `IONE_POLL_INTERVAL_SECS` (default 60), polls every active connector, runs rules, runs generator (capped to N streams per tick to bound cost).

**API additions:** `GET /api/v1/workspaces/:id/signals` with cursor pagination.

**UI:** Signals tab per workspace — reverse-chronological list with severity badge, title, body, expandable evidence (raw `stream_events`).

**Red:** test seeds a stream with an event whose `humidity=10`, a rule `humidity < 15 → flagged`, polls, asserts a `signal` row with `severity='flagged'` and `source='rule'` exists.

**Wiring checklist:** route registered, scheduler spawned in `main.rs`, rules method called from poller, generator method called from poller, UI renders severity badge.

**Exit criteria:** `cargo test --test phase05_signals`; with Ollama running, inducing a weather event produces both a rule-derived and a generator-derived signal within one poll tick.

**Risks:** generator latency and cost (local, but still 1–5s per call at 14B). Mitigate: cap per-tick budget, schedule at most one generator call per stream per tick, log model time.

**Non-goals:** critic (Phase 6), routing (Phase 7), embeddings for dedup (folded into Phase 6 or deferred).

## Phase 6 — Adversarial critic → survivors

**Goal:** every new signal is passed through the critic model; only survivors advance. Survivors carry verdict, rationale, confidence, chain-of-reasoning. UI adds a "Survivors" tab that shows the reasoning audit trail.

**Migration 0005:**
```sql
CREATE TYPE critic_verdict AS ENUM ('survive','reject','defer');

CREATE TABLE survivors (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  signal_id UUID NOT NULL UNIQUE REFERENCES signals(id) ON DELETE CASCADE,
  critic_model TEXT NOT NULL,
  verdict critic_verdict NOT NULL,
  rationale TEXT NOT NULL,
  confidence REAL NOT NULL,
  chain_of_reasoning JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Only rows with `verdict='survive'` are considered "live survivors"; `reject`/`defer` are retained for audit.

**Critic (src/services/critic.rs):** builds a red-team prompt around the signal: "You are a critic. Below is a proposed insight from a generator. Stress-test it against the evidence. Is it grounded, non-duplicative, and strong enough to notify a duty officer? Respond JSON: { verdict: survive|reject|defer, confidence: 0–1, rationale: string, steps: [string] }." Model `OLLAMA_CRITIC_MODEL` (default `phi4-reasoning:14b`). `steps` array populates `chain_of_reasoning`.

Critic runs as part of the scheduler tick, immediately after a signal is written.

**API additions:** `GET /api/v1/workspaces/:id/survivors?verdict=survive`.

**UI:** "Survivors" tab with severity badge, title, verdict chip, confidence bar, and an expandable chain-of-reasoning panel showing each step from `steps`.

**Red:** test seeds a signal with trivially wrong body ("humidity is 200%"), runs critic, asserts `verdict='reject'`. A second test with a grounded signal asserts `verdict='survive'` and `confidence > 0.5`.

**Wiring checklist:** route registered, critic called exactly once per signal, survivor row FK enforced unique per signal, UI chain-of-reasoning renders per-step.

**Exit criteria:** `cargo test --test phase06_critic`; UI shows survivor with expandable reasoning.

**Risks:** critic latency doubles per-signal cost; reasoning model output format drift. Mitigate: structured-output schema validation, retry once on parse fail, record `verdict='defer'` on persistent parse fail.

**Non-goals:** fine-tuning critic, multi-critic ensemble, calibration.

## Phase 7 — Routing classifier + role-scoped feed

**Goal:** each survivor receives a `routing_decision` from a small fast classifier model that assigns target kind (`feed|notification|draft|peer`) and target reference. The feed path is live end-to-end: role holders see a role-scoped feed in the UI.

**Migration 0006:**
```sql
CREATE TYPE routing_target AS ENUM ('feed','notification','draft','peer');

CREATE TABLE routing_decisions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  survivor_id UUID NOT NULL REFERENCES survivors(id) ON DELETE CASCADE,
  target_kind routing_target NOT NULL,
  target_ref JSONB NOT NULL,
  classifier_model TEXT NOT NULL,
  rationale TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX routing_decisions_survivor ON routing_decisions(survivor_id);
```

One survivor may have N routing decisions (feed for role X AND a draft for role Y, etc.).

**Classifier (src/services/router.rs):** prompt includes workspace roles and CoC levels; model `OLLAMA_ROUTER_MODEL` (default `qwen3:8b`); JSON schema enforces `targets: [{ kind, role_id?, peer_id?, severity, rationale }]`. Severity `routine→feed`, `flagged→notification` (stubbed until Phase 9 delivery), `command→draft` (human-approved; approvals Phase 9).

**API additions:** `GET /api/v1/workspaces/:id/feed?roleId=…` returns survivors with a `feed`-target decision for that role.

**UI:** a role picker in the workspace sidebar; the feed tab filters to current role. A survivor card shows its routing rationale on hover.

**Red:** test seeds a `command`-severity signal, runs full pipeline, asserts at least one `routing_decisions` row of `target_kind='draft'` AND no `feed` row for routine-only roles.

**Wiring checklist:** route registered, classifier runs exactly once per survivor, UI `roleId` query param reaches API.

**Exit criteria:** `cargo test --test phase07_routing`; UI role picker filters feed.

**Risks:** classifier output drift, contradictory routing. Mitigate: schema validation + fallback to severity→kind mapping on parse failure.

**Non-goals:** delivery (notifications + drafts are only recorded in Phase 7; they send in Phase 9).

## Phase 8 — OIDC auth + federated identity

**Goal:** log in with Keycloak; users' federated claims (role, CoC level, org) map to local `memberships` via `trust_issuers.claim_mapping`. Air-gapped mode (no IdP) still works via local users.

**Dependencies:**
```toml
openidconnect = "3"
axum-extra = { version = "0.9", features = ["cookie-signed"] }
jsonwebtoken = "9"
```

**docker-compose.yml addition:** keycloak service with a pre-imported `ione` realm (realm JSON committed under `infra/keycloak/realm.json`).

**Migration 0007:**
```sql
CREATE TABLE trust_issuers (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  issuer_url TEXT NOT NULL,
  audience TEXT NOT NULL,
  jwks_uri TEXT NOT NULL,
  claim_mapping JSONB NOT NULL,
  UNIQUE (org_id, issuer_url, audience)
);
```

**Auth flow:** `/auth/login?issuer=…` redirects to OIDC; `/auth/callback` sets a signed session cookie containing user id + active workspace + role claims. Middleware derives `AuthContext` from the cookie; unauthenticated requests get the seeded default user only when `IONE_AUTH_MODE=local` (air-gap).

**Claim mapping rules:** configurable JSON maps token claims to `{ role_name, coc_level, workspace_match }`. On callback, IONe upserts the user, resolves/creates the membership for the mapped role.

**API additions:** `/auth/login`, `/auth/callback`, `/auth/logout`, `GET /api/v1/me` returning `{ user, memberships, activeRole }`.

**UI:** login button; role switcher respects only roles actually held.

**Red:** integration test spins up a mock OIDC provider (one of `mockito`-style or a static test JWT with a local JWKS), asserts `/api/v1/me` returns the expected role after callback.

**Wiring checklist:** middleware applied to every non-auth route, `AuthContext` used in handlers, `trust_issuers` read on callback, UI sends cookie on `fetch`.

**Exit criteria:** `cargo test --test phase08_auth`; docker-compose Keycloak login round-trips; air-gap mode still serves the UI.

**Risks:** OIDC provider quirks; cookie sameness across docker-compose hostnames. Mitigate: `SameSite=Lax`, documented host config.

**Non-goals:** PIV/CAC smart-card (flagged as a v2 adapter), SCIM provisioning.

## Phase 9 — Delivery: notifications + human-approved drafts (Slack, SMTP)

**Goal:** Phase 7's routing decisions actually send. `notification` targets dispatch through outbound connectors (Slack webhook, SMTP); `draft` targets create `artifacts` with pending `approvals`; an approver can approve/reject, and approval triggers send. This is the first commands-down-via-connector path.

**Migration 0008:**
```sql
CREATE TYPE artifact_kind AS ENUM ('briefing','notification_draft','resource_order','message','report');
CREATE TYPE approval_status AS ENUM ('pending','approved','rejected');
CREATE TYPE actor_kind AS ENUM ('user','system','peer');

CREATE TABLE artifacts (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  kind artifact_kind NOT NULL,
  source_survivor_id UUID REFERENCES survivors(id) ON DELETE SET NULL,
  content JSONB NOT NULL,
  blob_ref TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE approvals (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  artifact_id UUID NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
  approver_user_id UUID REFERENCES users(id),
  status approval_status NOT NULL DEFAULT 'pending',
  comment TEXT,
  decided_at TIMESTAMPTZ
);
CREATE INDEX approvals_pending ON approvals(status) WHERE status = 'pending';

CREATE TABLE audit_events (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL,
  actor_kind actor_kind NOT NULL,
  actor_ref TEXT NOT NULL,
  verb TEXT NOT NULL,
  object_kind TEXT NOT NULL,
  object_id UUID,
  payload JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_events_workspace_created ON audit_events(workspace_id, created_at DESC);
```

**Connectors added:** `src/connectors/slack.rs` (webhook), `src/connectors/smtp.rs` (lettre crate). Both implement `Connector::invoke(op, args)` with `op="send"`.

**Dependencies:** `lettre = "0.11"`.

**Router updates:** `notification` target enqueues an `invoke("send", { channel, text })` call to the chosen outbound connector; outcome logged to `audit_events`. `draft` target creates an `artifact` + pending `approval`; on approval → same send path.

**API additions:** `GET /api/v1/workspaces/:id/artifacts`, `GET /api/v1/workspaces/:id/approvals?status=pending`, `POST /api/v1/approvals/:id` (decide).

**UI:** Approvals queue panel; clicking an item shows the draft + rationale + two buttons (Approve / Reject). Approving triggers a toast with the delivery result.

**Red:** test asserts that a `command`-severity survivor produces an artifact + pending approval, that approving it writes an `audit_events` row with `verb='delivered'`, and that the Slack HTTP call was made (mocked via `wiremock`).

**Wiring checklist:** route registered, approval decision triggers send exactly once, audit row written per send, UI queue polled (or SSE), Slack/SMTP config read from the connector row.

**Exit criteria:** `cargo test --test phase09_delivery`; manually approving a draft in the UI posts to a Slack test channel.

**Risks:** duplicate sends on retry. Mitigate: send is idempotency-keyed on `(approval_id)`; audit log is the gate.

**Non-goals:** rule-authorized auto-execution of commands-down (Phase 10), per-role escalation chains.

## Phase 10 — Rule-authorized auto-execution of commands-down

**Goal:** narrowly-scoped rules can authorize auto-execution (no human approval) for routine commands. Example: "auto-file spot weather request when forecast-update signal fires in workspace X."

**Schema changes:** `workspace.metadata.auto_exec_policies` — array of `{ trigger: {signal_match}, connector_id, op, args_template, rate_limit, severity_cap }`. `severity_cap='flagged'` means command-level severity cannot auto-execute under any policy.

**Services:** `src/services/approvals.rs` grows `evaluate_auto_exec(survivor) -> Option<AutoExecDecision>`. If matched, no `approval` row is created; invoke fires directly; two `audit_events` rows written: `auto_authorized` (actor=system) and `delivered`.

**UI:** Approvals panel shows a second "Auto-executed" tab that mirrors the approvals queue read-only. Badge count distinguishes.

**Red:** test seeds a policy, fires a matching survivor, asserts no `approval` row and two `audit_events` rows, and that `command`-severity cannot auto-execute even with a matching policy (`severity_cap` enforced).

**Wiring checklist:** policy evaluator called exactly once per survivor; invocation path shared with approved-draft path; severity_cap enforced; audit row of kind `auto_authorized` present.

**Exit criteria:** `cargo test --test phase10_auto_exec`.

**Risks:** this is the step with the highest real-world-consequence blast radius. Mitigate: per-policy `rate_limit` honored with a token bucket; severity_cap default `flagged`; policies off by default (must be explicitly added to `workspace.metadata`).

**Non-goals:** policy DSL UX (JSON in workspace metadata is the v1 surface).

## Phase 11 — IONe-as-MCP-server

**Goal:** expose IONe's capabilities as an MCP server so peer IONes (and arbitrary MCP clients) can call it. Operations: `list_workspaces`, `list_survivors`, `search_stream_events`, `deliver_notification`, `propose_artifact`.

**Dependencies:** `rmcp = "0.2"` (or the currently-leading Rust MCP crate; pin after a 30-minute evaluation of `rmcp` vs `mcp-sdk-rs` — whichever has an active release and stdio+HTTP transports).

**Transport:** HTTP+SSE mounted under `/mcp`. Stdio transport is not required in v1.

**Authorization:** MCP request authenticated by the session cookie (for same-origin calls) OR a bearer JWT from a trusted `trust_issuer` (for peer-to-peer calls). Each MCP tool validates CoC/sharing-policy.

**Red:** test uses the crate's client to call `list_workspaces` against the running server, asserts the returned shape.

**Wiring checklist:** MCP router mounted in `main.rs`, tool registrations grep-match the design-doc op list, auth middleware applied.

**Exit criteria:** `cargo test --test phase11_mcp_server`; `curl` against `/mcp/sse` returns a protocol handshake.

**Risks:** MCP crate churn. Mitigate: pin the version, encapsulate behind a thin trait so swapping is a 200-line change.

**Non-goals:** MCP resource/prompt surfaces beyond tools; non-default MCP transports.

## Phase 12 — MCP client + peer federation (single IONe talking to another)

**Goal:** one IONe can register a peer IONe, discover its capabilities, call `list_survivors` across the wire, and route survivors to a peer via the classifier's `peer` target kind.

**Migration 0009:**
```sql
CREATE TYPE peer_status AS ENUM ('active','paused','error');

CREATE TABLE peers (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  mcp_url TEXT NOT NULL,
  issuer_id UUID NOT NULL REFERENCES trust_issuers(id),
  sharing_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
  status peer_status NOT NULL DEFAULT 'active',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (mcp_url)
);
```

`src/connectors/mcp_client.rs` is a `Connector` impl that proxies to an MCP server. The connector is automatically synthesized for each peer; it is also reusable for non-IONe MCP servers.

**Sharing policy enforcement:** originating IONe's `sharing_policy` on the peer row gates what survivors may traverse; receiving IONe's routing classifier decides what its role holders see.

**UI:** Peers tab lists peers, shows last-heartbeat, allows adding/removing; a survivor card in the feed shows a "from: peer://lolo-nf" stripe when the source is a peer.

**Red:** spin up two in-process IONe instances in the test harness, federate them, assert a survivor on A is reachable from B subject to sharing policy.

**Wiring checklist:** MCP client invoked from scheduler; sharing policy validated before outbound; audit row for every cross-peer read/write.

**Exit criteria:** `cargo test --test phase12_peer`; docker-compose stand-up of two binaries on ports 3000/3001 demonstrably federates.

**Risks:** clock skew, JWKS caching, transient peer outages. Mitigate: 60s JWKS cache, exponential backoff, `peers.status='error'` surfacing.

**Non-goals:** multi-hop mesh routing; cross-peer write-back (Phase 13 scope).

## Phase 13 — Two-node federation demo: fire-ops scenario

**Goal:** a scripted demo that boots two IONes (Lolo NF, Bitterroot NF; interchangeable), loads FIRMS + S3 + IRWIN-read connectors, drives a "potential fire reported" scenario end-to-end (ingest → signal → critic → routing → peer notification → approved draft).

**New connectors:**
- `src/connectors/firms.rs` — NASA FIRMS area API (requires MAP_KEY env var) via OpenAPI-style adapter.
- `src/connectors/fs_s3.rs` — generic filesystem/S3 ingest for documents and imagery (aws-sdk-s3 crate, MinIO-compatible). Emits `stream_events` with `blob_ref`.
- `src/connectors/irwin.rs` — hand-written Rust IRWIN read (stub/mock for OSS release; real credential path documented but not required to run the demo).

**Dependencies:** `aws-sdk-s3 = "1"`, `aws-config = "1"`, feature-gated so non-S3 builds skip.

**Demo harness:** `scripts/demo.sh` brings up docker-compose, runs migrations on two DBs, seeds two workspaces, registers peer trust, posts a synthetic "fire reported" chat message that exercises the full loop and prints the resulting audit trail.

**Red:** `tests/phase13_demo.rs` — scripted end-to-end asserting at each step: event ingested, signal generated, critic survives, routing classifies (local feed + peer notification + draft), peer receives, Slack mock gets the notification, approval approves the draft, audit log has N rows.

**Wiring checklist:** demo script ends with `exit 0` only when every checkpoint passes; README has a one-command "run the demo" section.

**Exit criteria:** `./scripts/demo.sh` passes from a clean clone; video recording replaces any manual click-through.

**Risks:** demo fragility under Ollama cold-start and real-API rate limits. Mitigate: demo uses fixture data by default (`IONE_DEMO_MODE=fixtures`), real-API mode is opt-in.

**Non-goals:** pretty UI for the demo beyond what exists; custom branding; any marketing polish.

## Phase 14 — OSS release

**Goal:** first public release on GitHub under Apache 2.0.

**Deliverables:**
- `LICENSE` (Apache 2.0) at repo root (already added Phase 1; confirm).
- `README.md` — overview, quickstart, architecture links, demo instructions.
- `CHANGELOG.md` — first entry `v0.1.0`.
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`.
- GitHub Actions CI: fmt, clippy, test, build, docker image.
- `docker compose up` quickstart verified from a clean clone.
- Tagged `v0.1.0` release with release notes.

**Exit criteria:** `git tag v0.1.0 && gh release create v0.1.0` succeeds; CI is green.

**Risks:** premature attention without support bandwidth. Mitigate: README explicitly states "pre-alpha, evaluation only," issue tracker gets a triage schedule.

**Non-goals:** pypi/cargo publishing; helm chart (later).

## Cross-phase notes

**Testing discipline.** Every phase has at least one integration test under `tests/phaseNN_*.rs` that exercises the full vertical slice. Unit tests live next to the code they test; integration tests never stub DB or HTTP — they run against docker-compose services.

**SQLx offline.** `.sqlx/` is committed from Phase 2 onward. CI sets `SQLX_OFFLINE=true`.

**Observability.** `tracing` + JSON logs from Phase 1; request ids propagate through all handlers. OpenTelemetry exporter added in Phase 9 gated by `IONE_OTEL_ENDPOINT`.

**Contract fidelity.** Every PR that touches a field or API path must first update `md/design/ione-v1-contract.md`. CI runs a `grep`-based verification script against the contract.

**Build sequencing.** Phase dependency DAG:
```
P1 ── P2 ── P3 ── P4 ── P5 ── P6 ── P7 ── P9 ── P10
                                │
                                ├── P8 ── P11 ── P12 ── P13 ── P14
```
P8 can begin after P3 (it needs memberships). P11 can begin after P8 (it needs auth). P10 requires P9. P13 requires P7 + P9 + P12.

## Task Manifest

| Task | Agent | Files | Depends On | Gate | Status |
|------|-------|-------|------------|------|--------|
| T1: Phase 1 scaffold + chat proxy + static UI | claude-code | `Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/error.rs`, `src/state.rs`, `src/routes/{mod.rs,health.rs,chat.rs}`, `src/services/ollama.rs`, `static/{index.html,app.js,style.css}`, `tests/phase01_chat.rs`, `README.md`, `LICENSE` | — | `cargo check && cargo clippy -- -D warnings && cargo test --test phase01_chat -- --ignored` | completed |
| T2: Phase 2 Postgres + migrations 0001 + conversations | claude-code | `docker-compose.yml`, `.env.example`, `migrations/0001_initial.sql`, `src/routes/conversations.rs`, `src/state.rs`, `static/app.js` | T1 | `sqlx migrate run && cargo test --test phase02_conversations` | completed |
| T3: Phase 3 workspaces + roles + memberships migration 0002 | sql-coder | `migrations/0002_workspaces.sql`, `src/routes/workspaces.rs`, `static/app.js` | T2 | `sqlx migrate run && cargo test --test phase03_workspaces` | completed |
| T4: Phase 4 NWS connector + migration 0003 | codex | `migrations/0003_connectors.sql`, `src/connectors/{mod.rs,nws.rs}`, `src/routes/connectors.rs`, `tests/phase04_nws_connector.rs`, `static/app.js` | T3 | `cargo test --test phase04_nws_connector` | completed |
| T5: Phase 5 rules + generator + scheduler + migration 0004 | claude-code | `migrations/0004_signals.sql`, `src/services/{rules.rs,generator.rs}`, `src/routes/signals.rs`, `src/main.rs`, `tests/phase05_signals.rs`, `static/app.js` | T4 | `cargo test --test phase05_signals` | completed |
| T6: Phase 6 critic + migration 0005 | codex | `migrations/0005_survivors.sql`, `src/services/critic.rs`, `src/routes/survivors.rs`, `tests/phase06_critic.rs`, `static/app.js` | T5 | `cargo test --test phase06_critic` | completed |
| T7: Phase 7 routing classifier + migration 0006 | claude-code | `migrations/0006_routing.sql`, `src/services/router.rs`, `src/routes/signals.rs` (feed endpoint), `tests/phase07_routing.rs`, `static/app.js` | T6 | `cargo test --test phase07_routing` | completed |
| T8: Phase 8 OIDC auth + migration 0007 + Keycloak compose | claude-code | `migrations/0007_trust_issuers.sql`, `src/auth.rs`, `src/routes/auth.rs`, `docker-compose.yml`, `infra/keycloak/realm.json`, `tests/phase08_auth.rs`, `static/app.js` | T3 | `cargo test --test phase08_auth` | completed |
| T9: Phase 9 delivery (Slack+SMTP) + artifacts/approvals/audit migration 0008 | claude-code | `migrations/0008_artifacts_approvals_audit.sql`, `src/connectors/{slack.rs,smtp.rs}`, `src/routes/{artifacts.rs,approvals.rs}`, `src/services/router.rs` (delivery), `src/audit.rs`, `tests/phase09_delivery.rs`, `static/app.js` | T7 | `cargo test --test phase09_delivery` | completed |
| T10: Phase 10 auto-exec policies | codex | `src/services/approvals.rs`, `tests/phase10_auto_exec.rs`, `static/app.js` | T9 | `cargo test --test phase10_auto_exec` | completed |
| T11: Phase 11 IONe-as-MCP-server | claude-code | `Cargo.toml`, `src/mcp_server.rs`, `src/main.rs`, `tests/phase11_mcp_server.rs` | T8 | `cargo test --test phase11_mcp_server` | completed |
| T12: Phase 12 peer federation + MCP client + migration 0009 | claude-code | `migrations/0009_peers.sql`, `src/connectors/mcp_client.rs`, `src/routes/peers.rs`, `tests/phase12_peer.rs`, `static/app.js` | T11 | `cargo test --test phase12_peer` | completed |
| T13a: Phase 13 FIRMS connector | codex | `src/connectors/firms.rs`, test fixtures | T12 | `cargo test --test phase13_firms` | completed |
| T13b: Phase 13 S3 connector | codex | `Cargo.toml`, `src/connectors/fs_s3.rs`, test fixtures | T12 | `cargo test --test phase13_s3` | completed |
| T13c: Phase 13 IRWIN-read connector | codex | `src/connectors/irwin.rs`, fixtures | T12 | `cargo test --test phase13_irwin` | completed |
| T13d: Phase 13 two-node demo script | claude-code | `scripts/demo.sh`, `tests/phase13_demo.rs` | T13a, T13b, T13c | `./scripts/demo.sh` | completed |
| T14: Phase 14 OSS release (CI + README + docs + tag) | claude-code | `.github/workflows/ci.yml`, `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` | T13d | `gh workflow run ci` green on `v0.1.0` tag | completed |

Independent-parallelizable groups (for /co-code):
- Under T12: **T13a, T13b, T13c** are independent (no shared files), all `codex`, can dispatch in parallel.
- Under T3: **T8** starts early (requires only T3); it parallels T4–T7 on a separate branch.
