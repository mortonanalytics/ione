# Audit & T&E Event Export — Implementation Plan

**Design doc:** `md/design/audit-event-export.md`
**Shape:** medium (2+ layers, ~15 files; phases as vertical slices, no task manifest, no contract file)
**Review:** Codex review folded 2026-06-11 — centralized repo-layer scrub (finding 1), deterministic two-query export cursor (2), `id DESC` in keyset indexes + real-query EXPLAIN test (3), lateral-MIN recovery-gap contract + interleaved tests (4), probe-and-hide admin UI (5), legacy-row read-time scrub (policy gap).
**Stack:** Rust/Axum + Postgres (sqlx, embedded `sqlx::migrate!`) + static HTML/JS UI (`static/app.js`, `static/index.html`). Integration tests spawn the app against Postgres on `localhost:5433` (`tests/phase_*.rs` pattern, see `tests/phase_chart_aggregates.rs::spawn_app`).

## Dependencies

None. `base64 0.22`, `futures-util 0.3`, `tokio-stream 0.1` already in `Cargo.toml`.

## Resolved-at-plan-time facts (verified against working tree)

- Existing list route is `GET /api/v1/workspaces/:id/audit_events` (underscore), mounted at `src/routes/mod.rs:242`. We extend it in place; new endpoints use the hyphen convention of `event-aggregates` (`src/routes/mod.rs:156`).
- UI already has `tab-audit` / `panel-audit` with a polling fetch of `audit_events` (`static/app.js:4457-4513`). We extend that panel.
- Admin gate is `require_admin(&ctx, &state.pool)` (`src/auth.rs:294`, coc ≥ 80). Org-scope gate is `ensure_workspace_in_org` (`src/auth.rs`).
- Aggregate endpoint pattern to mirror: `src/routes/event_aggregates.rs` (90-day window, 1000-bucket cap, op dispatch) + `src/repos/stream_event_aggregate_repo.rs`.
- Secret-pattern reference: `is_secret_key` / `redact_value` in `src/models/connector.rs:46-66`.
- Migration numbering: next free is `0038`.
- Design open questions: none block code shape (peer-session verbs deferred, retention deferred, abstract wording is a proposal-editing task).

## Phases

### Phase 1 — Error-string scrub (design Slice 4; gates everything else)

**Goal:** error text persisted to `audit_events.payload` and `pipeline_events.detail` can no longer carry credentials or unbounded upstream bodies — from **any** write site, present or future.

**Approach (revised after Codex review):** the write-site inventory (`grep -rn 'error.to_string()\|err_msg\|err_str' src`) found persisted error strings beyond the two files the first draft named — at minimum `src/routes/connectors.rs:383` (pipeline event detail) and `src/services/auto_exec.rs:515,574` (audit payloads), in addition to `src/services/delivery.rs` and `src/services/scheduler.rs`. Per-site wrapping is fragile (the next write site silently regresses), so scrubbing is **centralized at the repo write layer**: every payload/detail JSON value passes through a recursive scrub of `"error"`-keyed string fields before INSERT. Write sites are not individually edited.

**Files:**
- `src/util/redact.rs` — **create.** `scrub_error_text()` + `scrub_error_fields()` + unit tests in-module.
- `src/util/mod.rs` — add `pub mod redact;`
- `src/repos/audit_event_repo.rs` — in `insert_with_foreign_tenant` (the single INSERT choke point — `insert` delegates to it), apply `scrub_error_fields(&mut payload)` before binding.
- `src/repos/pipeline_event_repo.rs` — same in the append/insert method for `detail`.

**Code shapes:**
```rust
// src/util/redact.rs
/// Strip credential-bearing substrings and truncate.
pub fn scrub_error_text(input: &str) -> String {
    // 1. URL userinfo: scheme://user:pass@host -> scheme://[redacted]@host
    // 2. Key-value secrets (case-insensitive): (authorization|token|key|secret|password)[=:\s]\S+ -> $1=[redacted]
    // 3. Truncate to 256 chars on a char boundary, append "…" if truncated.
}

/// Recursively walk a JSON value; for every object entry whose key is "error"
/// and whose value is a string, replace it with scrub_error_text(value).
/// Applied at the repo layer to every audit payload / pipeline detail write.
pub fn scrub_error_fields(value: &mut serde_json::Value);
```
Implement with plain string scanning (`regex` is not in `Cargo.toml`; the patterns are simple enough).

**Legacy rows (policy decision, per review):** rows written before this phase remain unsanitized at rest. v1 handles this at **read time**: the Phase 2 list response and Phase 4 export response pass each row's payload through `scrub_error_fields` before serialization. DB contents are not rewritten (no backfill migration); the bulk-readable surfaces are clean regardless of row age. This is the AU-9 story: write-time scrub stops new at-rest secrets, read-time scrub covers history.

**Gate:** `cargo test --lib redact` (unit tests: userinfo URL, `Authorization: Bearer x`, `token=x`, 4KB truncation, nested `{"a":{"error":"…"}}` object walk) and `cargo clippy --all-targets -- -D warnings`.

**Acceptance (design AC-8):** integration-level — insert an audit event whose payload contains `{"error": "https://user:secret@host/path … <4KB body>"}` via `AuditEventRepo::insert` and read the row back: stored value contains neither `secret` nor `user:` and is ≤ 257 chars. (Covers all write sites by construction.)

---

### Phase 2 — Filterable audit list + indexes + Audit-panel filters (design Slice 1)

**Goal:** a workspace member can filter and page the audit trail in the UI and via the API; queries hit the new indexes.

**Files:**
- `migrations/0038_audit_export_indexes.sql` — **create.** All four indexes (the `pipeline_events` one lands here too; one migration, used by Phase 3):
```sql
-- id DESC included so the keyset ORDER BY (created_at DESC, id DESC) and the
-- cursor predicate (created_at, id) < ($1, $2) are fully index-served (Codex finding 3).
CREATE INDEX audit_events_ws_actor_kind_created ON audit_events (workspace_id, actor_kind, created_at DESC, id DESC);
CREATE INDEX audit_events_ws_verb_created       ON audit_events (workspace_id, verb, created_at DESC, id DESC);
CREATE INDEX audit_events_ws_actor_ref_created  ON audit_events (workspace_id, actor_ref, created_at DESC, id DESC);
CREATE INDEX pipeline_events_ws_stage_time      ON pipeline_events (workspace_id, stage, occurred_at DESC);
```
- `src/repos/audit_event_repo.rs` — add filter struct + keyset-paged query:
```rust
#[derive(Debug, Default, Clone)]
pub struct AuditEventFilter {
    pub actor_kind: Option<ActorKind>,
    pub actor_ref: Option<String>,
    pub verbs: Vec<String>,              // WHERE verb = ANY($n)
    pub object_kind: Option<String>,
    pub object_id: Option<Uuid>,
    pub foreign_tenant_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// Keyset page: WHERE (created_at, id) < (cursor_ts, cursor_id) ORDER BY created_at DESC, id DESC.
/// All filter values are bind parameters (QueryBuilder); no interpolation.
pub async fn list_filtered(
    &self,
    workspace_id: Uuid,
    org_id: Uuid,                        // enforced via JOIN workspaces w ON w.id=$ws AND w.org_id=$org
    filter: &AuditEventFilter,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,                          // pre-clamped 1..=200 by the route
) -> anyhow::Result<Vec<AuditEvent>>
```
- `src/routes/audit_events.rs` — extend `list_audit_events` with a `Query<AuditEventsQuery>` (serde struct mirroring the contract table; `verb` repeatable via `Vec<String>`), cursor codec, and `{ items, next_cursor }` response; each item's payload passes through `scrub_error_fields` before serialization (Phase 1 read-time backstop for legacy rows). Cursor: `base64(URL_SAFE_NO_PAD, "{created_at_rfc3339}|{id}")`; decode failure → 400. **Backward compatible:** all params optional; no params ⇒ first page of 200, `next_cursor` present — the existing UI ignores the new field until updated below.
- `src/routes/mod.rs` — no route change (path unchanged); verify only.
- `static/index.html` — filter controls inside `panel-audit`: `<select id="audit-filter-actor-kind">` (all/user/system/peer), `<input id="audit-filter-verb">`, `<select id="audit-filter-window">` (1h/24h/7d/30d), `<button id="audit-load-more">`.
- `static/app.js` — `loadAuditTrail()` (line ~4460): build query string from the filter controls, render `next_cursor`-driven "Load more", keep the existing poll-on-tab behavior for the unfiltered first page.
- `md/requirements/active/audit-event-export.md` — **create** (seeds the project's requirements source-of-truth directory, which does not exist yet). Content: the four-endpoint API contract table, per-op response shapes, and authz tiers copied from the design doc's "API contracts" section, with a header noting which phase ships each endpoint. Phases 3–4 update this file **only if** an implemented contract deviates from it; the PR must not merge with the requirements doc contradicting shipped behavior.
- `tests/audit_export_integration.rs` — **create** (shared by Phases 2–4; `spawn_app` copied from `tests/phase_chart_aggregates.rs`; the `_integration` name suffix is the repo-standard pattern the pre-PR coverage gate recognizes — do not name it `phase_*`). This phase: seed 350 events (120 `peer_tool_executed`), assert AC-1 (filter + two-page cursor walk), AC-6 cross-org 404, and AC-10 `EXPLAIN` assertion:
```rust
// AC-10: EXPLAIN the REAL cursor query shape (with id tiebreaker + cursor
// predicate — Codex finding 3), assert index scan, no Seq Scan and no Sort node.
let plan: Vec<String> = sqlx::query_scalar(
    "EXPLAIN SELECT id FROM audit_events
     WHERE workspace_id=$1 AND verb=$2 AND created_at>=$3
       AND (created_at, id) < ($4, $5)
     ORDER BY created_at DESC, id DESC LIMIT 100")
    .bind(ws).bind("peer_tool_executed").bind(since).bind(cursor_ts).bind(cursor_id)
    .fetch_all(&pool).await?;
let text = plan.join("\n");
assert!(text.contains("Index"), "expected index scan: {text}");
assert!(!text.contains("Seq Scan"), "unexpected seq scan: {text}");
```

**Gate:** `cargo test --test audit_export_integration phase2` (name phase-2 tests with a `phase2_` prefix) — plus `cargo clippy --all-targets -- -D warnings`.

**Acceptance:** AC-1, AC-6 (404 half), AC-10 pass as named tests.

---

### Phase 3 — Aggregates + Audit-panel chart/stat (design Slice 2)

**Goal:** an admin sees interaction counts over time and recovery-gap stats in the Audit panel; both metrics are computable via the public API.

**Files:**
- `src/repos/audit_event_aggregate_repo.rs` — **create.** Mirrors `StreamEventAggregateRepo`'s interface shape (do **not** generalize it — scope joins differ):
```rust
pub struct AuditEventAggregateRepo { pool: PgPool }

/// GROUP BY date_trunc($bucket, created_at), <group_col>.
/// bucket from allow-list {minute,hour,day,week} (format!-ed like the existing
/// repo's bucket_expr); group_col from allow-list {actor_kind, verb, actor_ref}
/// mapped to a hard-coded column ident — never from raw user input.
pub async fn count_by_bucket(&self, workspace_id: Uuid, org_id: Uuid,
    bucket: &str, group_col: GroupCol, filter: &AuditEventFilter)
    -> anyhow::Result<Vec<BucketCountRow>>   // { key, bucket_start, count }

/// GROUP BY actor_ref ORDER BY count DESC LIMIT 200.
pub async fn count_by_actor(&self, workspace_id: Uuid, org_id: Uuid,
    filter: &AuditEventFilter) -> anyhow::Result<Vec<ActorCountRow>>  // { key, count }
```
- `src/repos/pipeline_event_aggregate_repo.rs` — **create.** Contract (Codex finding 4): for each fault event, the recovery point is the **earliest later `publish_started` on the same connector**, ignoring any intervening stages (`first_event`, repeated `stall`/`error`, other streams on the connector). Plain `LEAD()` (next-event-of-any-stage) is wrong — use a lateral MIN:
```rust
/// SELECT f.connector_id, f.stage AS from_stage, f.occurred_at,
///        EXTRACT(EPOCH FROM (r.recovered_at - f.occurred_at)) AS gap_seconds
/// FROM pipeline_events f
/// JOIN workspaces w ON w.id = f.workspace_id AND w.org_id = $org
/// JOIN LATERAL (
///   SELECT MIN(occurred_at) AS recovered_at FROM pipeline_events r
///   WHERE r.workspace_id = f.workspace_id AND r.connector_id = f.connector_id
///     AND r.stage = 'publish_started' AND r.occurred_at > f.occurred_at
/// ) r ON r.recovered_at IS NOT NULL
/// WHERE f.workspace_id = $ws AND f.stage IN ('stall','error')
///   AND f.connector_id IS NOT NULL          -- NULL-connector rows deliberately excluded
///   AND f.occurred_at >= $since AND f.occurred_at < $until
/// ORDER BY f.occurred_at LIMIT 10_000
pub async fn recovery_gaps(&self, workspace_id: Uuid, org_id: Uuid,
    connector_id: Option<Uuid>, since: DateTime<Utc>, until: DateTime<Utc>)
    -> anyhow::Result<Vec<RecoveryGapRow>>   // { connector_id, gap_seconds, from_stage, occurred_at }
```
- `src/repos/mod.rs` — export both.
- `src/routes/audit_aggregates.rs` — **create.** Both handlers (`get_audit_aggregates`, `get_pipeline_aggregates`). Validation per contract table: `op` allow-list; `bucket`+`group_by` required when `op=count_by_bucket`, **400 if present** when `op=count_by_actor`; window ≤ 90d; bucket count ≤ 1000 (reuse the arithmetic from `event_aggregates.rs:75-84`). Both handlers: `ensure_workspace_in_org` then `require_admin`. Response shapes exactly per design "Per-op response shapes". `summary` percentiles (p50/p90/max) computed in Rust over the returned gaps (≤10k items — no second query).
- `src/routes/mod.rs` — mount `GET /api/v1/workspaces/:id/audit-aggregates` and `GET /api/v1/workspaces/:id/pipeline-aggregates` next to the `event-aggregates` route (line ~156).
- `static/index.html` — inside `panel-audit`: a stat strip (`<div id="audit-stats">` — total interactions, recovery p50/p90) and a counts-by-hour bar rendered with the existing chart panel path **only if trivially reusable**; otherwise a plain HTML/CSS bar list (design permits "summary stat row"; do not build a new chart type).
- `static/app.js` — **admin detection (Codex finding 5, resolved):** `/api/v1/me` carries no admin/CoC flag and `loadMe()` retains no role metadata — do **not** extend the API. Use probe-and-hide: on audit-tab activation fetch `audit-aggregates`; on 403 set a module-level `auditAdminDenied = true`, hide the stat strip and (Phase 4) the export button, and skip further admin-only fetches for the session. On 200, render. The server-side `require_admin` gate remains the actual protection.
- `tests/audit_export_integration.rs` — `phase3_` tests: AC-2 (count_by_bucket sums per actor), AC-3 (count_by_actor 5/3 ordering + `bucket=hour` → 400), AC-4 (seed `error` at T, `publish_started` at T+90s ⇒ `gap_seconds==90`, `from_stage=="error"`, `occurred_at==T`, `summary.count==1`), AC-6 403-half for both endpoints with a non-admin member. Plus interleaved-stage tests (Codex finding 4): (a) `error` at T, `first_event` at T+30s, `publish_started` at T+90s ⇒ still `gap_seconds==90`; (b) two connectors with overlapping fault windows ⇒ each gap pairs within its own connector; (c) fault with no later `publish_started` ⇒ no row emitted; (d) fault row with NULL `connector_id` ⇒ excluded.

**Gate:** `cargo test --test audit_export_integration phase3` + clippy.

**Acceptance:** AC-2, AC-3, AC-4, AC-6 (403 half) pass as named tests.

---

### Phase 4 — Bulk NDJSON export (design Slice 3)

**Goal:** an admin exports the filtered audit trail as NDJSON from the panel; caps and concurrency limits enforced.

**Export algorithm (deterministic two-query design, Codex finding 2):**
1. Clamp `until = min(until, now())` at request time. `audit_events` is append-only, so the window is frozen from here on — the two queries below see identical data.
2. **Key query** (index-only, cheap): `SELECT created_at, id FROM audit_events WHERE <filters + cursor predicate> ORDER BY created_at DESC, id DESC LIMIT 10_001`. If 10,001 keys return, the export is truncated: `X-Next-Cursor` = encoded key of row **10,000** (the last row this response includes; the continuation predicate `(created_at, id) < cursor` then starts at row 10,001). Drop key 10,001.
3. **Row stream**: same WHERE + `(created_at, id) <= key[0] AND (created_at, id) >= key[last]` (bounds from the key query), `ORDER BY created_at DESC, id DESC` — streamed via `.fetch()`, each row's payload passed through `scrub_error_fields` (Phase 1 read-time backstop), serialized as one NDJSON line.

A `COUNT(*)` pre-check is explicitly **not** used — it cannot produce the cursor token. The key query is the single source of both the truncation decision and the cursor value, computed before headers are sent.

**Files:**
- `src/repos/audit_event_repo.rs` — add `keyset_page(...) -> Vec<(DateTime<Utc>, Uuid)>` (key query above) and `stream_between_keys(...) -> impl Stream<Item = sqlx::Result<AuditEvent>>` (row stream above; same WHERE-builder as `list_filtered`).
- `src/routes/audit_export.rs` — **create.** `get_audit_export`: `ensure_workspace_in_org` → `require_admin` → validate `since`+`until` required, span ≤ 90d, clamp `until` → acquire per-org export permit → key query → headers → stream:
```rust
// Per-org single-flight: DashMap<Uuid, ()> on AppState; entry() insert or 429.
// Guard struct removes the entry on Drop so a dropped connection frees the slot.
// Response: header("content-type", "application/x-ndjson"),
//           header("x-next-cursor", <key[9_999] encoded>) iff key query hit 10_001,
//           Body::from_stream(rows.map(|r| serde_json::to_string(&scrubbed(r)) + "\n"))
```
- `src/state.rs` — add `pub export_locks: Arc<DashMap<Uuid, ()>>` to `AppState` (DashMap already used for `peer_manifest_cache`).
- `src/routes/mod.rs` — mount `GET /api/v1/workspaces/:id/audit-export`.
- `static/index.html` — `<button id="audit-export-btn" hidden>Export NDJSON</button>` in `panel-audit`.
- `static/app.js` — export click → fetch with current filters, save blob (`URL.createObjectURL`), follow `X-Next-Cursor` until absent; button visibility driven by the Phase 3 `auditAdminDenied` probe flag (hidden until the first aggregates fetch succeeds).
- `tests/audit_export_integration.rs` — `phase4_` tests: AC-5 (seed 10,500 rows → 10,000 NDJSON lines + header, cursor walk returns 500), AC-7 (91-day span → 400; missing `since` → 400), AC-9 (hold one streaming response open, second request → 429), AC-6 403 for non-admin.

**Gate:** `cargo test --test audit_export_integration phase4` + clippy + full `cargo test --test audit_export_integration` (all phases green together).

**Acceptance:** AC-5, AC-7, AC-9, AC-6 (export 403) pass as named tests.

---

## Acceptance-criteria → phase map (self-review, step 7)

| Design AC | Phase | Gate test |
|---|---|---|
| 1 (filtered list + cursor) | 2 | `phase2_filtered_list_cursor_walk` |
| 2 (count_by_bucket) | 3 | `phase3_count_by_bucket_sums` |
| 3 (count_by_actor + bucket→400) | 3 | `phase3_count_by_actor` |
| 4 (recovery_gap fields) | 3 | `phase3_recovery_gap` |
| 5 (export 10k + cursor) | 4 | `phase4_export_truncation_cursor` |
| 6 (403 non-admin; 404 cross-org) | 2/3/4 | `phase2_cross_org_404`, `phase3_non_admin_403`, `phase4_non_admin_403` |
| 7 (export param validation 400) | 4 | `phase4_export_validation` |
| 8 (scrub) | 1 | `redact` unit tests + `phase1_repo_write_scrub` (insert-and-read-back through the repo choke point) |
| 9 (concurrent export 429) | 4 | `phase4_concurrent_export_429` |
| 10 (EXPLAIN index scan) | 2 | `phase2_explain_index_scan` |

Files cited all exist except those marked **create**. Phases are vertical slices (each ships API+UI+test together; Phase 1 is write-path-only by design). Gates are concrete commands. No parallel agents → no contract file, no manifest.

## Notes for /code-the-plan

- **If a previous run already created `tests/phase_audit_export.rs`:** rename it (`mv tests/phase_audit_export.rs tests/audit_export_integration.rs`) rather than creating a second file — the `_integration` suffix is what the pre-PR coverage gate recognizes.

- Run integration tests against the dev Postgres (`postgres://ione:ione@localhost:5433/ione` per test default) — **environment preflight: confirm the container is up before Phase 2** (`pg_isready -h localhost -p 5433` or equivalent); if absent, stop and report.
- `TRUNCATE` list in `spawn_app` must include `pipeline_events` for Phase 3 seeds.
- Per the backlog, mark the P6 item **Partial — pending founder walkthrough** when code-complete; "shipped" requires the walkthrough.
