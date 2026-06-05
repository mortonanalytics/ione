# Rule-Engine Offline Reliability — Implementation Plan

**Design doc:** [md/design/rule-engine-offline-reliability.md](../design/rule-engine-offline-reliability.md)
**Shape:** large (~20 files, 3 vertical slices) — phases + Task Manifest, **no contract file** (all three slices serialize on `src/services/rules.rs` + `src/services/scheduler.rs`; there is no disjoint 3-way parallel split, so a separate contract is a third stale source).
**Stack:** Rust + axum + sqlx (Postgres) backend; vanilla-JS demo shell (`static/`). Migration runner: `sqlx migrate run`. Verify: `SQLX_OFFLINE=true cargo check`, `cargo clippy`, `cargo test <name> -- --ignored` (integration tests require `DATABASE_URL`; CI provides a pg16 service), `npx tsc`/none for static JS.

## Phasing rationale

Ordered so each phase ships one end-to-end capability and respects the `rules.rs` evolution:
1. **Phase 1 = Slice 3** (deterministic survivor) — the actual offline unblock; smallest; independent of diagnostics. This is the "if forced to cut, ship this" core.
2. **Phase 2 = Slice 1** (diagnostics infra + UI) — refactors `evaluate_workspace` to return a structured report and builds the snapshot/endpoint/UI. Foundation that Slice 2's `type_mismatch` reporting plugs into.
3. **Phase 3 = Slice 2** (schema-typed normalization + rule validation) — adds connector field-types + normalization that **emits `type_mismatch` into Phase 2's report**, plus PATCH-time rule validation. Last because AC-4b depends on the Phase-2 diagnostics surface.

All three phases are sequential (shared `rules.rs`/`scheduler.rs`); run one agent at a time. The Task Manifest reflects this.

## Dependencies

None new. Reuses `evalexpr` (already a dep), `sqlx`, `serde_json`. No new crate.

---

## Phase 1 — Deterministic survivor for rule signals (Slice 3)

**Goal:** A rule match writes its own `survive` survivor atomically with the signal, so the `signal → survivor → router → draft/approval` loop completes offline (Ollama down).

**Files:**
- `src/services/rules.rs` — on a matched event, replace the single `signal_repo.insert(...)` call with an **atomic transaction** that inserts the signal then the paired deterministic survivor, then commits. Keep the surrounding loop/idempotency (`exists_by_title_for_events`) unchanged.
- `tests/phase05_signals.rs` — add offline loop tests (AC-1, AC-2, AC-9). May add a small helper that runs one scheduler tick with `IONE_SKIP_LIVE` unset but Ollama unreachable (the critic's defer path is exercised and must be a no-op for the rule signal).

**Code shapes:**
```rust
// src/services/rules.rs — inside `if matched { ... }`, replacing the signal_repo.insert call.
// Atomic: signal + deterministic survivor in one txn (design Slice 3 + Risk 4 orphan-avoidance).
let mut tx = pool.begin().await.context("begin rule-signal txn")?;
let signal_id: Uuid = sqlx::query_scalar(
    "INSERT INTO signals
       (workspace_id, source, title, body, evidence, severity, generator_model, approval_required)
     VALUES ($1, 'rule', $2, $3, $4, $5, NULL, $6)
     RETURNING id",
)
.bind(workspace_id).bind(&rule.title).bind(&format!("Rule matched: {}", rule.when))
.bind(&evidence).bind(severity.clone()).bind(approval_required)
.fetch_one(&mut *tx).await.context("insert rule signal")?;

sqlx::query(
    "INSERT INTO survivors
       (signal_id, critic_model, verdict, rationale, confidence, chain_of_reasoning)
     VALUES ($1, 'rule-engine', 'survive'::critic_verdict, $2, 1.0, '[]'::jsonb)",
)
.bind(signal_id).bind(&format!("rule matched: {}", rule.when))
.execute(&mut *tx).await.context("insert deterministic survivor")?;

tx.commit().await.context("commit rule-signal txn")?;
inserted += 1;
```
- `'rule-engine'` is the sentinel `critic_model`; `confidence 1.0`; `chain_of_reasoning '[]'` (column is `JSONB NOT NULL DEFAULT '[]'`).
- The existing critic stage (`scheduler.rs:429-446`, `LEFT JOIN survivors WHERE sv.id IS NULL`) skips this signal because it already has a survivor; `survivors.signal_id` is `UNIQUE` as the backstop.

**Gate:** `SQLX_OFFLINE=true cargo check && SQLX_OFFLINE=true cargo clippy --all-targets -- -D warnings` then `DATABASE_URL=$DATABASE_URL cargo test rule_deterministic_survivor -- --ignored --test-threads=1` (new test). Add an AC-8 guard assertion in the same file: a `source='generator'` signal with only a `defer` survivor still appears in `GET …/signals`.
**Acceptance (AC-1, AC-2, AC-9):** after one offline tick, for the M6.4 `command` rule signal: `signals` has a `source='rule'` row; `survivors` has exactly **one** row for it with `verdict='survive'`, `critic_model='rule-engine'`; `routing_decisions` has a `target_kind='draft'` row; an `artifacts` pending-approval row exists and **no** notification audit row (`delivered`) was written.

---

## Phase 2 — Rule-evaluation diagnostics (Slice 1)

**Goal:** Every tick records per-rule outcome; `GET …/rule-diagnostics` exposes it; the demo-shell Signals panel renders benign-vs-broken; a `rule_diagnostic` pipeline event drives live refresh.

**Files to create:**
- `migrations/0035_rule_diagnostics.sql` — snapshot table (one row per workspace).
- `migrations/0036_pipeline_events_rule_diagnostic_stage.sql` — **alter the `pipeline_events.stage` CHECK constraint** to allow `'rule_diagnostic'` (the existing unnamed `CHECK (stage IN (...))` at `0012_pipeline_events.sql:8` rejects any new stage at INSERT time — without this, every `emit_stage(... RuleDiagnostic ...)` fails). Postgres auto-names the constraint `pipeline_events_stage_check`.
- `src/models/rule_diagnostic.rs` — report/diagnostic/status types.
- `src/repos/rule_diagnostics_repo.rs` — upsert + get + clear.
- `src/routes/rule_diagnostics.rs` — `GET` handler.

**Files to modify:**
- `src/services/rules.rs` — change `evaluate_workspace` return from `anyhow::Result<usize>` to `anyhow::Result<RuleEvalReport>` (additive; callers that ignore the value still compile — verify `tests/phase05_signals.rs:316` only `.expect()`s). **Compile each rule's `when` once** via `evalexpr::build_operator_tree` (a compile failure → `parse_error`, skip the rule's events), then per event evaluate and **classify the `EvalexprError`** rather than blanket-logging it (current code at `rules.rs:122-134` warns+skips silently): `VariableIdentifierNotFound` → skip-reason code `field_absent`; `TypeError | WrongTypeCombination | ExpectedNumber | ExpectedFloat | ExpectedString` → skip-reason code `type_mismatch`. This is what makes an **undeclared** field's numeric-vs-string failure surface as `type_mismatch` (design Slice 2, "undeclared field" bullet) instead of a silent skip — Phase 3 only adds *proactive* declared-type normalization on top. Rule status = worst across its events: `parse_error` > `type_mismatch` > `no_events` (stream had zero events) > `ok`. Whole-array deser failure (`rules` present but not a valid rule array) → single synthetic entry `rules_unparseable`. `stream_not_found` when the rule's stream resolves to nothing.
- `src/services/scheduler.rs` — at stage (b), take the `RuleEvalReport`: if it has **≥1 rule diagnostic**, **upsert** the snapshot; if the workspace has **no rules** (report diagnostics empty), **delete** the snapshot row (`RuleDiagnosticsRepo::clear`) so a stale row never lingers after rules are removed by any path. Emit `emit_stage(..., PipelineEventStage::RuleDiagnostic, Some(summary))` once per workspace per tick only when diagnostics are non-empty. Update `handle_signal_stage` (or its call site) to accept the report's `inserted` count for the existing "produced signals" log/first-signal emit.
- `src/models/pipeline_event.rs` — add `RuleDiagnostic` variant → `"rule_diagnostic"`.
- `src/models/mod.rs`, `src/repos/mod.rs` — export new model + repo.
- `src/routes/mod.rs` — `pub mod rule_diagnostics;` and `.route("/api/v1/workspaces/:id/rule-diagnostics", get(rule_diagnostics::get_diagnostics))` in the same authenticated group as `…/signals` (line ~187).
- `static/index.html` — add a `#rule-diagnostics` region inside `#panel-signals` with its own `aria-live="polite"`.
- `static/app.js` — fetch `…/rule-diagnostics` in `loadSignals()`; render the diagnostic block (benign = muted/info; broken = warning icon **+** text label; `no_events` → button switching to Connectors tab); refresh the block when a `rule_diagnostic` SSE event arrives (extend `handlePipelineEvent`).
- `static/style.css` — diagnostic-block styles (reuse severity-chip token pattern; respect `prefers-reduced-motion`).
- `tests/` — new `tests/phase05b_rule_diagnostics.rs` (AC-5, AC-6, AC-10).

**Code shapes:**
```sql
-- migrations/0035_rule_diagnostics.sql
CREATE TABLE rule_diagnostics (
    workspace_id UUID PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    evaluated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    items        JSONB       NOT NULL DEFAULT '[]'::jsonb   -- RuleDiagnostic[]
);

-- migrations/0036_pipeline_events_rule_diagnostic_stage.sql
ALTER TABLE pipeline_events DROP CONSTRAINT pipeline_events_stage_check;
ALTER TABLE pipeline_events ADD  CONSTRAINT pipeline_events_stage_check
    CHECK (stage IN ('publish_started','first_event','first_signal',
                     'first_survivor','first_decision','stall','error','rule_diagnostic'));
```
`skip_reasons[].code` vocabulary: `field_absent`, `type_mismatch`, `parse_error`, `stream_not_found`, `rules_unparseable` (the per-event detail strings carry the field pointer / evalexpr message).
```rust
// src/models/rule_diagnostic.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagStatus { Ok, StreamNotFound, NoEvents, ParseError, TypeMismatch, RulesUnparseable }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkipReason { pub code: String, pub detail: String, pub count: i64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDiagnostic {
    pub rule_index: i64,
    pub rule_title: String,
    pub stream: String,
    pub status: DiagStatus,
    pub events_evaluated: i64,
    pub match_count: i64,
    pub skip_reasons: Vec<SkipReason>,   // cap 5 distinct codes
}

// internal — returned by evaluate_workspace
pub struct RuleEvalReport { pub inserted: usize, pub diagnostics: Vec<RuleDiagnostic> }
```
```rust
// src/routes/rule_diagnostics.rs
pub async fn get_diagnostics(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    ensure_workspace_in_org(&state.pool, workspace_id, ctx.org_id).await?;
    let snap = RuleDiagnosticsRepo::new(state.pool.clone()).get(workspace_id).await?;
    // snap: Option<(DateTime<Utc>, Vec<RuleDiagnostic>)>
    Ok(Json(json!({
        "evaluatedAt": snap.as_ref().map(|s| s.0),
        "items": snap.map(|s| s.1).unwrap_or_default(),
    })))
}
```
```rust
// src/repos/rule_diagnostics_repo.rs — upsert (last-write-wins), get, clear
// upsert: INSERT ... ON CONFLICT (workspace_id) DO UPDATE SET evaluated_at = now(), items = $2
// clear (used by Phase 3 PATCH): DELETE FROM rule_diagnostics WHERE workspace_id = $1
```

**Gate:** `SQLX_OFFLINE=true cargo check && SQLX_OFFLINE=true cargo clippy --all-targets -- -D warnings` then `DATABASE_URL=$DATABASE_URL cargo test rule_diagnostics -- --ignored --test-threads=1` (integration tests are serial + need a live pg per `CONTRIBUTING.md`). **Include a DB test that appends a `rule_diagnostic` pipeline event and reads it back** — proves the `0036` constraint migration actually admits the new stage (guards the Codex High finding). Run `cargo sqlx prepare` after the migrations so offline check passes (`.sqlx/` committed).
**Acceptance (AC-5, AC-6, AC-10):** `GET …/rule-diagnostics` returns: for an unknown-stream rule → item `status="stream_not_found"`, non-empty `skipReasons`, `matchCount=0`; for a valid below-threshold rule → `status="ok"`, `eventsEvaluated>0`, `matchCount=0`; for `metadata.rules` set to a non-array/garbage → single item `status="rules_unparseable"`.

---

## Phase 3 — Schema-declared field types + rule validation (Slice 2)

**Goal:** Connector declares field types; the rule engine normalizes payload values to the declared type (string-typed fields never numerically coerced); malformed rules are rejected at save with `422`; saving rules clears the diagnostics snapshot.

**Files:**
- `src/connectors/geojson_poll.rs` — add optional `field_types: Option<HashMap<String, FieldType>>` to `GeoJsonPollConfig` (JSON-pointer key → type), with a `FieldType { Number, String, Boolean }` enum (`#[serde(rename_all="snake_case")]`). Validate in `GeoJsonPollConfig::validate()`: each key is a valid JSON pointer (`validate_json_pointer`). **Note:** the *route* validator (`validate/geojson_poll.rs:13`) fetches the live feed after `from_config`, so it is **not** usable as a no-network test of `field_types`. Test the declaration with a **config-only unit test** calling `GeoJsonPollConnector::from_config(...)` directly (a bad pointer → `Err`), in the existing `#[cfg(test)] mod tests` in `geojson_poll.rs`.
- `src/services/rules.rs` — before evaluating a rule's events, read the resolved connector's `config.field_types` (the connector is already in scope when the stream is found). Add a `normalize_into_context` step: for each declared field, convert the event payload value to the declared type when building the evalexpr context; on a value that cannot be normalized (`number` field with non-numeric string, `boolean` field with non-bool string), **skip the event and record a `type_mismatch`** diagnostic naming the pointer. This is the *proactive* path; it composes with Phase 2's eval-error classifier, which already maps `TypeError`/`ExpectedNumber` runtime failures on **undeclared** fields to `type_mismatch`. Undeclared fields keep native-type behavior at context-build time (errors surface via the classifier, not silently).
- `src/routes/workspaces.rs` — in `patch_workspace`, when `req.metadata` contains `rules`: deserialize to the rule shape (a whole-array deser failure → **422** too), and for each rule **build the evalexpr operator tree** for `when` (`evalexpr::build_operator_tree`); on parse error return a **structured 422**. On success (and only when `rules` key is present in the patch), call `RuleDiagnosticsRepo::clear(id)` after the metadata write. **AC-10 split (explicit):** the API write path *rejects* malformed rules here (422), so `rules_unparseable` is only reachable at eval time for metadata that bypassed this path (legacy rows, direct-DB seeds, tests inserting via SQL) — both behaviors coexist and are tested separately.
- `src/error.rs` — the existing `AppError::UnprocessableEntity(String)` renders a fixed `{"error":"unprocessable_entity","message": msg}` body (confirmed at `error.rs:77-83`); it **cannot** carry a discrete `ruleIndex` field. To honor the contract's `{ error, ruleIndex, detail }`, add one variant `UnprocessableEntityJson(serde_json::Value)` → `(StatusCode::UNPROCESSABLE_ENTITY, Json(value))`. ~4 lines, mirrors the existing arm.
- **UI: API-only (no static change).** The demo shell has **no rule-authoring surface** — grep confirms no `metadata.rules` PATCH path in `static/`. Rules are authored by the integrator via the API (the API-first persona in the design's UX analysis). The `422` is therefore an **API contract guarantee tested at the API**, not a shell interaction. *Building a rule-editor UI is out of scope for this plan* — if one is wanted later it is a separate feature with its own design. (Phase 2's read-only diagnostic block stays — that surface, `loadSignals`/`#panel-signals`, already exists.)
- `tests/` — extend `tests/phase05b_rule_diagnostics.rs` / `phase05_signals.rs` with AC-3, AC-4, AC-4b, AC-7 (AC-7 asserts the `422` at the API).

**Code shapes:**
```rust
// src/connectors/geojson_poll.rs
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType { Number, String, Boolean }
// added to GeoJsonPollConfig:
//   #[serde(default)] field_types: Option<std::collections::HashMap<String, FieldType>>,

// src/services/rules.rs — declared-type normalization (pointer "/properties/mag" -> ctx key "payload.properties.mag")
fn normalize_value(declared: FieldType, v: &serde_json::Value) -> Result<EvalValue, ()> {
    match declared {
        FieldType::Number => match v {
            serde_json::Value::Number(n) => n.as_f64().map(EvalValue::Float).ok_or(()),
            serde_json::Value::String(s) => s.parse::<f64>().ok()
                .filter(|f| f.is_finite()).map(EvalValue::Float).ok_or(()),
            _ => Err(()),
        },
        FieldType::String => Ok(EvalValue::String(match v {
            serde_json::Value::String(s) => s.clone(), other => other.to_string() })),
        FieldType::Boolean => match v {
            serde_json::Value::Bool(b) => Ok(EvalValue::Boolean(*b)),
            serde_json::Value::String(s) if s == "true" || s == "false" =>
                Ok(EvalValue::Boolean(s == "true")), _ => Err(()) },
    }
}
// Err(()) for a declared field => record SkipReason{code:"type_mismatch", detail:"<pointer>"} and skip event.
```
```rust
// src/routes/workspaces.rs — rule validation (422), inside patch_workspace before persisting
if let Some(rules) = req.metadata.get("rules") {
    let parsed: Vec<RuleShape> = serde_json::from_value(rules.clone())
        .map_err(|e| AppError::Unprocessable(json!({"error":"invalid_rules","detail":e.to_string()})))?;
    for (i, r) in parsed.iter().enumerate() {
        evalexpr::build_operator_tree(&r.when).map_err(|e| AppError::UnprocessableEntityJson(
            json!({"error":"invalid_rule_expression","ruleIndex": i, "detail": e.to_string()})))?;
    }
}
// ...after update_metadata succeeds:
if req.metadata.get("rules").is_some() { RuleDiagnosticsRepo::new(state.pool.clone()).clear(id).await?; }
```
- A malformed `rules` array (deser fails → AC-10's `rules_unparseable` at eval time) should also 422 here via the same `UnprocessableEntityJson`.

**Gate:** `SQLX_OFFLINE=true cargo check && SQLX_OFFLINE=true cargo clippy --all-targets -- -D warnings`; config-only validation via `cargo test geojson_poll_field_types` (no DB/network); then `DATABASE_URL=$DATABASE_URL cargo test rule_typed -- --ignored --test-threads=1` and `DATABASE_URL=$DATABASE_URL cargo test rule_validation -- --ignored --test-threads=1`.
**Acceptance (AC-3, AC-4, AC-4b, AC-7):** `number`-declared `mag="6.4"` (string) → `source='rule'` signal created; `string`-declared `code` with `=="01234"` and `=="12345"` → match; `number`-declared `mag="high"` → no signal + `GET …/rule-diagnostics` item `status="type_mismatch"` naming `/properties/mag`; `PATCH …/workspaces/:id` with `when:"payload.mag $$$"` → `422`, body `ruleIndex=0`, metadata unchanged.

---

## Task Manifest

Mostly sequential — T1, T2, T4 each touch `src/services/rules.rs` and must serialize (T1→T2→T4). T3 (`static/*`) and T5 (`routes/workspaces.rs`, `error.rs`) have disjoint file sets and both depend only on T2 (T5 also on T4), so they may run after their deps without conflicting with each other.

| Task | Agent | Files | Depends On | Gate |
|------|-------|-------|------------|------|
| T1: Deterministic survivor (Phase 1) | claude-code | `src/services/rules.rs`, `tests/phase05_signals.rs` | — | `cargo test rule_deterministic_survivor -- --ignored --test-threads=1` |
| T2: Diagnostics migrations + model + repo + route (Phase 2 backend) | claude-code | `migrations/0035_rule_diagnostics.sql`, `migrations/0036_pipeline_events_rule_diagnostic_stage.sql`, `src/models/rule_diagnostic.rs`, `src/models/mod.rs`, `src/models/pipeline_event.rs`, `src/repos/rule_diagnostics_repo.rs`, `src/repos/mod.rs`, `src/routes/rule_diagnostics.rs`, `src/routes/mod.rs`, `src/services/rules.rs`, `src/services/scheduler.rs` | T1 | `cargo test rule_diagnostics -- --ignored --test-threads=1` (incl. the `rule_diagnostic` append/read DB test) |
| T3: Diagnostics UI block (Phase 2 frontend) | claude-code | `static/index.html`, `static/app.js`, `static/style.css` | T2 | manual: diagnostic block renders benign vs broken; `rule_diagnostic` SSE refreshes |
| T4: Connector field-types + normalization (Phase 3) | claude-code | `src/connectors/geojson_poll.rs`, `src/services/rules.rs` | T2 | `cargo test geojson_poll_field_types` + `cargo test rule_typed -- --ignored --test-threads=1` |
| T5: PATCH rule validation + snapshot clear (Phase 3, API-only) | claude-code | `src/routes/workspaces.rs`, `src/error.rs` (add `UnprocessableEntityJson` variant) | T2, T4 | `cargo test rule_validation -- --ignored --test-threads=1` |

All `claude-code` (every task edits existing code with live callers — `rules.rs`, `scheduler.rs`, route registration). After each task: confirm `git diff --stat` non-empty and `SQLX_OFFLINE=true cargo check` passes; run `cargo sqlx prepare` after T2's migration and commit `.sqlx/`.

## Self-review

1. **Every AC → a gate?** AC-1/2/9 → T1 gate; AC-5/6/10 → T2 gate; AC-3/4/4b → T4 gate; AC-7 → T5 gate; AC-8 (`/signals` not critic-gated) is a non-regression — add to T1's test file as a guard assertion. ✓ (AC-8 noted; add explicit assertion in T1.)
2. **Every file exists or is listed to-create?** New files flagged "to create" in Phase 2; all modified files verified present (rules.rs, scheduler.rs, pipeline_event.rs, workspaces.rs, geojson_poll.rs, routes/mod.rs, static/*). ✓
3. **Vertical slices?** Phase 1 advances the loop; Phase 2 makes failures visible; Phase 3 makes typed matching deterministic — each end-to-end. ✓
4. **Concrete gate commands?** Named `cargo test <filter> -- --ignored` per phase. ✓
5. **Parallel tasks disjoint?** T1/T2/T4 serialize on `rules.rs`; T3 (`static/*`) and T5 (`routes/workspaces.rs`+`error.rs`) are disjoint and may overlap after their deps. ✓

**Carried defaults (not blocking):** skip-reason cap = 5 distinct codes; diagnostics snapshot retains no history (last-write-wins); `rule_diagnostic` emitted once per workspace per tick. All from the design doc.
