# geojson_poll Connector — Implementation Plan

**Design doc:** `md/design/geojson-poll-connector.md`
**Shape:** medium (db + api, ~14 files, 3 phases as vertical slices + one scaffolding phase)
**Stack:** Rust / Axum / sqlx (Postgres), `sqlx::migrate!("./migrations")` at startup and in tests; integration tests mirror `tests/phase_event_layers.rs` (`#[ignore]`, DB at `:5433`); HTTP mocked with `wiremock` (loopback is SSRF-allowed).

This is **P7-supporting** (Stream P — IONe substrate ingest for the GroundPulse/Epicenter demo path).

## Dependencies
None. `wiremock 0.6` already in dev-deps; `chrono`, `reqwest`, `serde_json`, `url` already present.

## Phasing rationale
Design Slice 3 (dedup) has no user-visible outcome without Slice 1 (the connector that sets a dedup key), so they are folded into one vertical phase. Slice 2 (PUT endpoint) is independent and smaller, so it ships first after a pure-refactor Phase 0. Phase 0 is scaffolding only (5 files, no new feature): it extracts shared code and fixes the confirmed `view_config` RETURNING bug.

---

## Phase 0 — Shared scaffolding + cross-org hardening

**Goal:** Extract & harden the reusable pieces both later phases need, fix the `Stream.view_config` round-trip bug, and close the pre-existing cross-org gap on the stream-id routes this feature touches — before `view_config` is added to any serialized response.

**Files:**
- `src/util/url_guard.rs` — **create**. Move `parse_and_validate_url`, `ensure_safe_url`, `host_is_http_allowed` from `src/connectors/openapi.rs:615-660`; make them `pub(crate)`. **Harden (F3):** after the scheme match, reject all IPv4 link-local (`169.254.0.0/16`, via `Ipv4Addr::is_link_local`) and IPv6 link-local (`fe80::/10`, via `Ipv6Addr` segment check) for **every** scheme — not just the exact metadata IP, and not skipped for `https`. Private/loopback over http/https stays allowed (on-prem peers). Add a `pub(crate) fn guarded_client(timeout_ms: u64) -> reqwest::Client` builder: `redirect::Policy::none()`, timeout, polite UA — the single client both the connector poll and the validate path use.
- `src/util/mod.rs` — **modify**. `pub mod url_guard;`
- `src/connectors/openapi.rs` — **modify**. Delete the three moved fns; `use crate::util::url_guard::{parse_and_validate_url, ...};`. Keep `MAX_RESPONSE_BYTES`. (OpenAPI now also blocks link-local — intended.)
- `src/services/event_layers.rs` — **modify**. Public wrapper over private `CompiledConfig::parse` (event_layers.rs:92):
  ```rust
  pub fn validate_view_config(vc: &serde_json::Value) -> Result<(), String> {
      CompiledConfig::parse(vc).map(|_| ())
  }
  ```
- `src/models/stream.rs` — **modify**. Add `pub view_config: Option<serde_json::Value>,` to `Stream`.
- `src/repos/stream_repo.rs` — **modify**. Add `view_config` to `RETURNING`/`SELECT` in `upsert_named:28`, `list:42`, `get:55`. **Add org-scoped methods (F1):**
  ```rust
  // SELECT s.* FROM streams s JOIN connectors c ON c.id=s.connector_id
  //   JOIN workspaces w ON w.id=c.workspace_id WHERE s.id=$1 AND w.org_id=$2
  pub async fn get_in_org(&self, id: Uuid, org_id: Uuid) -> anyhow::Result<Option<Stream>>;
  // same join, WHERE c.id=$1 AND w.org_id=$2 — org-scoped stream list for a connector
  pub async fn list_in_org(&self, connector_id: Uuid, org_id: Uuid) -> anyhow::Result<Vec<Stream>>;
  ```
- `src/routes/connectors.rs` — **modify (F1).** `list_streams` and `poll_stream` gain `Extension(ctx): Extension<AuthContext>`; `list_streams` calls `list_in_org(connector_id, ctx.org_id)`; `do_poll_stream` takes `org_id` and uses `get_in_org(stream_id, ctx.org_id)`, returning `AppError::NotFound` on `None` **before** any connector build or fetch.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_event_layers -- --ignored --test-threads=1
DATABASE_URL=... cargo test --test phase13_connectors -- --ignored --test-threads=1   # existing connector/stream route tests still green
```
**Acceptance:**
- AC-10: repo test `upsert_named_returns_view_config` — `upsert_named(.., Some(json!({...})))` returns a `Stream` with `view_config == Some(..)`.
- AC-14: cross-org `GET /connectors/:id/streams` omits the foreign stream; cross-org `POST /streams/:id/poll` → 404 and `stream_events` count unchanged (no fetch). Add to `tests/phase_geojson_poll.rs`.
- SSRF matrix (AC-6 cases b/c/d, unit tests on `url_guard`): `https://169.254.169.254/`, `http://169.254.10.10/`, `http://[fe80::1]/` all rejected.

---

## Phase 1 — Slice 2: runtime `view_config` authoring (`PUT /streams/:id/view-config`)

**Goal:** An operator can set a stream's map-render config via one request; invalid configs are rejected 422, cross-org 404.

**Files:**
- `src/repos/stream_repo.rs` — **modify**. `get_in_org` already added in Phase 0. Add:
  ```rust
  // UPDATE streams SET view_config=$2 WHERE id=$1 RETURNING <full cols incl view_config>
  pub async fn update_view_config(&self, id: Uuid, view_config: Option<serde_json::Value>) -> anyhow::Result<Stream>;
  ```
- `src/routes/connectors.rs` — **modify**. Add handler:
  ```rust
  pub async fn put_stream_view_config(
      State(state): State<AppState>,
      Extension(ctx): Extension<AuthContext>,
      Path(stream_id): Path<Uuid>,
      Json(body): Json<serde_json::Value>,
  ) -> Result<Json<PutViewConfigResponse>, AppError>
  // PutViewConfigResponse { id: Uuid, view_config: serde_json::Value }  (serde camelCase)
  ```
  Flow: `validate_view_config(&body).map_err(AppError::UnprocessableEntity)?;` → `get_in_org(stream_id, ctx.org_id)?` → `None` ⇒ `AppError::NotFound` → `update_view_config(stream_id, Some(body.clone()))` → **emit audit event (F7)** via `AuditEventRepo::insert(Some(workspace_id), ctx actor, "stream.view_config.updated", "stream", Some(stream_id), json!({"old_hash":..,"new_hash":..}))` (workspace_id resolved from the stream's connector; hashes via a stable digest of the JSON) → return `{ id, view_config: body }`.
- `src/routes/mod.rs` — **modify**. After line 132 (`/api/v1/streams/:id/poll`):
  ```rust
  .route("/api/v1/streams/:id/view-config", put(connectors::put_stream_view_config))
  ```
  Add `put` to the `axum::routing` import if absent.
- `tests/phase_geojson_poll.rs` — **create** (shared file with Phase 2). Add `#[ignore]` tests `view_config_put_*`.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_geojson_poll view_config -- --ignored --test-threads=1
```
**Acceptance (AC-7/8/9):**
- AC-8: PUT a valid `{lon_pointer, lat_pointer}` body → 200, response `view_config` equals body; `SELECT view_config FROM streams WHERE id=$id` equals body.
- AC-7: PUT body missing `lat_pointer` → 422; DB `view_config` unchanged.
- AC-9: stream in org A, request as org B → 404; DB unchanged.

---

## Phase 2 — Slice 1+3: `geojson_poll` connector with natural-key dedup

**Goal:** A `geojson_poll` connector config ingests a GeoJSON/JSON feed (epoch-ms timestamps, type filter, natural-key dedup) into `stream_events`, rendering on the existing event-layers map — no per-source Rust.

**Files:**
- `migrations/0031_stream_events_dedup_key.sql` — **create**:
  ```sql
  ALTER TABLE stream_events ADD COLUMN dedup_key TEXT;
  CREATE UNIQUE INDEX stream_events_stream_dedup_key
      ON stream_events (stream_id, dedup_key) WHERE dedup_key IS NOT NULL;
  ```
- `src/connectors/mod.rs` — **modify**. Add field to `StreamEventInput`:
  ```rust
  pub dedup_key: Option<String>,
  ```
  Add `pub mod geojson_poll;` and a dispatch arm in `build_with_pool` (after the `irwin` arm, before the `bail!`):
  ```rust
  if kind_hint == "geojson_poll" || name_lower.starts_with("geojson") {
      let c = geojson_poll::GeoJsonPollConnector::from_config(config)?;
      return Ok(Box::new(c));
  }
  ```
  Update the `bail!` message to include `'geojson_poll'`.
- `src/connectors/firms.rs`, `fs_s3.rs`, `irwin.rs`, `nws.rs`, `mcp_client.rs`, `openapi.rs` — **modify**. Each `StreamEventInput { .. }` literal (firms:128, fs_s3:175 & 228, irwin:135, nws:166, openapi:805, mcp_client:151) gains `dedup_key: None,`.
- `src/repos/stream_event_repo.rs` — **modify (F4).** Add the dedup-aware insert returning an explicit outcome; keep `insert_if_absent` for the webhook path:
  ```rust
  pub enum InsertOutcome { Inserted, Updated, Duplicate }
  // dedup_key.is_some() => ON CONFLICT (stream_id, dedup_key) WHERE dedup_key IS NOT NULL
  //   DO UPDATE SET payload=EXCLUDED.payload, observed_at=EXCLUDED.observed_at
  //   RETURNING (xmax = 0) AS inserted  -> map to Inserted/Updated
  // dedup_key.is_none() => existing (stream_id, observed_at) DO NOTHING -> Inserted/Duplicate
  pub async fn insert_event(&self, stream_id: Uuid, payload: serde_json::Value,
                            observed_at: DateTime<Utc>, dedup_key: Option<&str>) -> anyhow::Result<InsertOutcome>;
  ```
- `src/services/scheduler.rs` (661), `src/routes/connectors.rs` (280, 442) — **modify (F4).** Route connector-driven inserts through `insert_event(.., evt.dedup_key.as_deref())`; count only `InsertOutcome::Inserted` toward `ingested`/`inserted_count`/`FirstEvent` (an `Updated` recurring-feed row must **not** read as fresh ingestion). Leave `src/routes/peers.rs:348` (webhook push) on `insert_if_absent`.
- `src/connectors/geojson_poll.rs` — **create**. Implements `ConnectorImpl`. Config (deserialized from `connectors.config`, `#[serde(deny_unknown_fields)]` to match openapi style):
  ```
  kind, feed_url, stream_name, items_pointer(default "/features"),
  observed_at_pointer(Option), observed_at_format(enum rfc3339|epoch_ms|epoch_s|none),
  dedup_pointer(Option), type_filter(Option {pointer, allow:Vec<String>}),
  max_items(Option<usize> 1..=10000), view_config(Option<Value>), timeout_ms(default 15000, 1..=30000)
  ```
  `from_config` validates: `validate_json_pointer` on every pointer field; `observed_at_pointer` required unless format=`none` (else error → surfaces 422 at create); `type_filter` all-or-nothing; `feed_url` via `url_guard::parse_and_validate_url`; `view_config` via `event_layers::validate_view_config`. `default_streams()` returns one `StreamDescriptor { name: stream_name, schema, view_config }` — this is the **only** place `view_config` is written (F2: poll does **not** re-assert it). `poll()`: `parse_and_validate_url` → `url_guard::guarded_client(timeout_ms)` GET (redirects disabled, polite UA) → enforce 2 MiB `MAX_RESPONSE_BYTES` cap on the body → parse → resolve `items_pointer` to array → truncate `max_items` → per feature: type-filter (skip on miss); timestamp per format (epoch_ms via `Utc.timestamp_millis_opt`; skip+warn on parse fail for non-`none`); **dedup_key (F5):** if `dedup_pointer` set, resolve to a string — only a non-empty `String` with `len ≤ 512` is kept; missing/empty/non-string/oversized ⇒ skip the feature with a warning (no fallback); push `StreamEventInput { payload: feature, observed_at, dedup_key }`. `next_cursor: None`.
- `src/connectors/validate/geojson_poll.rs` — **create**. `pub async fn validate(config: &Value) -> ValidateResult` mirroring `validate/firms.rs`: build the config (reusing `GeoJsonPollConnector::from_config` for shape validation → map err to `ValidateErr`), fetch the feed with `url_guard::guarded_client` (**not** `short_client` — F3: redirects disabled + same 2 MiB cap as poll), resolve `items_pointer`, return `ValidateOk { sample: json!({ "featureCount": n }) }`.
- `src/connectors/validate/mod.rs` — **modify**. `pub mod geojson_poll;`; add `Some("geojson_poll") => geojson_poll::validate(config).await,` to `dispatch`; add `candidate == "geojson_poll" || name.starts_with("geojson")` arm to `rust_native_provider`; extend the known-providers error strings.
- `tests/phase_geojson_poll.rs` — **modify**. Add connector tests (unit tests for pure helpers may also live inline in `geojson_poll.rs`, mirroring `openapi.rs`).
- `tests/fixtures/usgs_*.json` (or inline `json!` in the test) — **create**. A USGS-shaped FeatureCollection + a second structurally-distinct feed for AC-5.

**Gate:**
```
cargo clippy --all-targets -- -D warnings
DATABASE_URL=postgres://ione:ione@localhost:5433/ione cargo test --test phase_geojson_poll -- --ignored --test-threads=1
```
**Acceptance (AC-1..6, AC-11..13):**
- AC-1: poll a wiremock USGS fixture → `stream_events` count > 0, rows have `dedup_key` non-null, `payload` has `mag` + coordinates.
- AC-2: feature `properties.time=1779991039445`, format `epoch_ms` → stored `observed_at` == `2026-05-28T17:57:19Z` (±1s).
- AC-3: serve fixture twice, same `id`, changed `mag` → `count=1` for that dedup_key, `payload->>'mag'` is the second value; the second poll's reported new-event count is 0 (`Updated`, not `Inserted`).
- AC-4: fixture with one `earthquake` + one `quarry blast`, `type_filter` allow `[earthquake]` → exactly 1 event.
- AC-5: second distinct GeoJSON fixture wired by config only ingests with mapped fields; `git diff --name-only` shows no new connector `.rs` beyond `geojson_poll.rs`.
- AC-6: SSRF matrix at the connector create route — `http://169.254.169.254/...`, `https://169.254.169.254/`, `http://169.254.10.10/`, `http://[fe80::1]/` each → 422 and 0 `connectors` rows; a feed that 302s to the metadata IP errors at validate/poll (redirects disabled). (Guard-level cases also covered by Phase 0 url_guard unit tests.)
- AC-11 (e2e map render): poll the USGS fixture, then `GET /api/v1/workspaces/:id/event-layers` returns a point feature with the fixture's coordinates and the mapped `magnitude`.
- AC-12 (PUT survives poll): PUT a custom `view_config` on a `geojson_poll` stream, then poll → DB `view_config` still equals the PUT body.
- AC-13 (dedup_key edge cases): fixture with one valid `id`, one missing `id`, one `id:""` → `count=1` after poll.

---

## Requirements impact (post-merge, not code)
Per design § Requirements impact — update `md/design/app-integration-playbook.md` (add `geojson_poll` to the connector catalog + `observed_at_format` options), note the SSRF-guard extraction in `md/design/openapi-connectors.md`, and move the P1 backlog items to done in `md/plans/infrastructure-backlog.md`. Defer to `/preflight`/`/pr` via `update-requirements`.

---

## Self-review

1. **Every design AC maps to a phase gate?** Yes — AC-10 + AC-14 + SSRF guard-unit cases → Phase 0; AC-7/8/9 → Phase 1; AC-1..6, AC-11, AC-12, AC-13 → Phase 2. All in the acceptance blocks.
2. **Every file exists now or is listed to create?** Verified: all `modify` targets exist (confirmed via Read) — incl. `src/repos/audit_event_repo.rs` (`insert` signature confirmed) and `src/repos/connector_repo.rs` for workspace resolution; `create` targets are `url_guard.rs`, migration `0031`, `geojson_poll.rs`, `validate/geojson_poll.rs`, `tests/phase_geojson_poll.rs`, fixtures.
3. **Phases vertical, not layer stacks?** Phase 0 is the one allowed pure-scaffolding phase. Phase 1 ships PUT end-to-end (repo+route+test). Phase 2 ships the connector end-to-end (migration+plumbing+connector+validate+test). Slices 1 and 3 folded because dedup has no standalone outcome.
4. **Gates concrete?** Yes — named `cargo test --test phase_geojson_poll <filter> -- --ignored --test-threads=1` + clippy, matching the repo's DB-test convention.
5. **Parallel disjoint sets?** N/A — sequential (medium plan, solo). Phase 1 and Phase 2 touch a common file (`src/routes/connectors.rs`), so they must not run in parallel anyway.

**Note for implementer:** `StreamEventInput` gains a required field; the 7 listed construction sites must all be updated in Phase 2 or the crate won't compile. `--test-threads=1` is mandatory for the DB-backed tests (shared-DB truncate races otherwise).
