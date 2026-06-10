# IONe App On-Ramp — Implementation Plan

**Date:** 2026-05-31
**Status:** Proposed. Awaiting go/no-go.
**Outcome ID:** P7 (IONe v0.1) — supporting; integration-fabric framing per [.claude/rules/path-2-stream-p.md](../../.claude/rules/path-2-stream-p.md).
**Designs:** [app-integration-state-machine.md](../design/app-integration-state-machine.md), [building-on-ione.md](../playbooks/building-on-ione.md), [app-integration-playbook.md](../design/app-integration-playbook.md).

## Problem

IONe federates apps that **already exist** as live MCP peers (GroundPulse, TerraYield). It has **no path** for the dominant real shape — the artifact dashboard (doi-ss-ping, doi-reclamation): precomputed, read-only, file-based, no server. Today the only two routes to a panel are a live-poll connector or a live MCP peer; "here is a Parquet I computed" fits neither. Result: every demo rebuilds charts/tables/maps/508/serving from scratch. Goal: a fast on-ramp so a new app (= a demo = a production app) renders in IONe in hours, **without weakening the federation thesis** — every primitive here also speeds peer onboarding.

## Strategic guardrail

Per the Path-2 rule, frame each task as an **on-ramp / ingest primitive that serves all reference apps**, not a standalone-product feature. The artifact connector and embedded-app SDK make *peer* onboarding faster too (a peer can stage an artifact before its MCP server exists). If a task only helps IONe-as-standalone, cut it.

## Task Manifest

| ID | Task | Layer | Depends on | Disposition |
|----|------|-------|-----------|-------------|
| **ONR-001a** | **Contract-hardening slice (do BEFORE the 9–10d clock):** artifact row identity (generate a per-row `dedup_key`, reuse the EXISTING `(stream_id, dedup_key)` upsert — no new migration); workspace-scoped ingest API; `panels.maps`/`panels.eventLayers` summary contract; `view_config.charts[]` contract; CSV/GeoJSON-first + explicit Parquet feature/dependency decision; tests for duplicate timestamps + no-peer map visibility | db + api + contract | — | **Scheduled — Phase 0 (prerequisite)** |
| ONR-001 | Artifact connector (file → stream_events + view_config) | db + api + connector | ONR-001a | **Scheduled — Phase 1** |
| ONR-002 | Embedded-app SDK + `ione new-app` scaffold (generalize loopback peer) | api + tooling | ONR-001 | **Scheduled — Phase 2** |
| ONR-003 | Single-tenant demo auth profile (clean buyer login) | auth | — | **Scheduled — Phase 2** |
| ONR-004 | Map tab for no-peer workspaces + static map overlay support (baked PNG/tiles, not only XYZ) | api + ui | ONR-001a | **Scheduled — Phase 3** |
| ONR-005 | Port doi-reclamation (chart + table + gauge) onto IONe as reference + measure time-to-demo | integration | ONR-001..004 | **Scheduled — Phase 3** |
| ONR-006 | Doc set adoption (state machine + playbook) + agent test loop | docs | — | **Done this session** (this plan + the two design docs) |
| ONR-007 | `notifications/*` reception (live peer push channel) | api | — | **Deferred — re-enter after ONR-005** (separate infra-backlog item; not on the artifact on-ramp critical path) |
| ONR-008 | Enforce the approval floor at signal creation: a `flagged`/`command` signal from any source (rule, generator, webhook) sets `approval_required` so auto-exec is guaranteed to skip it | api + services | — | **Scheduled — Phase 0 (correctness; small)** |
| CAU-001..006 | Connector / data-source auth (API key, basic, OAuth client-creds + auth-code) | see [connector-auth-plan.md](connector-auth-plan.md) | — | **Scheduled — parallel track**; Tier-1 needed for any gated artifact source |

No silent drops: every assessment recommendation maps to ONR-001..005; the two Codex hardening items map to ONR-001a (ingest/UI/contract) and ONR-008 (approval-floor); data-source auth is the CAU-* track; the prior infra priority (`notifications/*`) is explicitly deferred (ONR-007).

---

## Phase 0 — Contract hardening (prerequisite; do before the clock starts)

Per the Codex repo-grounded review, three contract gaps would break the artifact path if discovered mid-implementation. Close them first.

**ONR-001a — Artifact identity, panel-summary, and chart contracts.**
- **Row identity / dedup (smaller than first thought — mechanism already exists).** The dedup path is already built: `insert_event` (`src/repos/stream_event_repo.rs`) takes a `dedup_key` and upserts via `ON CONFLICT (stream_id, dedup_key) DO UPDATE` (migration `0031_stream_events_dedup_key.sql`, partial unique index `WHERE dedup_key IS NOT NULL`). The danger is the **fallback path**: when `dedup_key` is absent, the insert falls to `ON CONFLICT (stream_id, observed_at) DO NOTHING` (`migrations/0003_connectors.sql`, index `stream_events_stream_observed_unique`), which **silently drops** rows sharing a timestamp — exactly what daily county/metric artifacts produce. **Fix:** the artifact connector must **always generate and supply a per-row `dedup_key`** (e.g. content hash of the record, or `artifact_id + row_index`). No constraint change needed; the existing `(stream_id, observed_at)` index is harmless once every artifact row carries a `dedup_key`. Optionally make a missing `dedup_key` on the artifact path a hard error rather than a silent drop.
- **Panel-summary contract.** Add `panels.maps` / `panels.eventLayers` counts to `GET /api/v1/workspaces/:id` (`src/routes/workspaces.rs`) so a no-peer artifact workspace with event layers is discoverable. (UI gating in ONR-004.)
- **Chart contract.** Define `view_config.charts[]` so the artifact path can express specific chart specs (and later gauges), not only line/aggregate panels derived from `property_fields`.
- **Format scope.** CSV + GeoJSON first (dependency-light); make Parquet an explicit, feature-flagged decision (Arrow/polars adds binary weight to the Rust build) rather than assuming it.
- **Tests:** duplicate-timestamp import (idempotent, no error); no-peer workspace with event-layer stream surfaces a map tab.
- **Effort:** ~1.5–2 days. Blocks ONR-001/004/005.

**ONR-008 — Approval-floor enforcement.** Set `approval_required` for any `flagged`/`command` signal at creation, regardless of source, so the documented invariant ("flagged/command always bypasses auto-exec") matches the code. Today only webhook ingest sets it; rule/generator `flagged` signals can slip past auto-exec. Small, correctness-only. *Confirm the delivery/auto_exec/webhook_ingress behavior at implementation time.*

## Phase 1 — Artifact ingest (the unlock)

**ONR-001 — Artifact connector.** Highest leverage; everything else builds on it.

- **DB:** builds on ONR-001a's dedup migration. Reuse `streams` + `stream_events`; add `streams.source_kind = 'artifact'` and an `artifact_ref` (S3 key or inline upload). No new render tables — the projection layer (`stream_event_aggregate_repo`, chart/table/event-layer services) already consumes `stream_events` + `view_config`. Imports are idempotent on `(stream_id, dedup_key)`.
- **Connector:** `src/connectors/artifact.rs` — parse CSV / Parquet (via `polars` or `arrow`, already in the workspace via doi-reclamation precedent) / GeoJSON into `stream_events` rows: each record → `{payload: jsonb, observed_at}`. Honor `view_config` JSON Pointers for `observed_at`, geometry, and `property_fields`.
- **API:** extend `POST /api/v1/connectors/validate` to dry-run an artifact (schema + pointer resolution + row count) and a create endpoint that ingests. Reject malformed pointers up front (the Machine-1 `ArtifactStaged → Error` edge).
- **Acceptance:** load doi-reclamation's `series_daily.parquet` + a hand-written `view_config`; `GET /chart-data` and `/table-data` return correct rows; the panel renders in a workspace. **UI pairing:** none new — existing panels consume it (backend/UI parity satisfied because the read path already exists).
- **Effort:** ~2–3 days.

---

## Phase 2 — Make it scaffoldable and demo-able

**ONR-002 — Embedded-app SDK + scaffold.** Convert the `IONE_SEED_DEMO` loopback mock peer (`POST /demo/mcp`, `src/demo/seeder.rs`) from a demo-only hack into a supported pattern.

- Extract the loopback peer into a documented **in-process app** mode: an app can register as a local peer served by IONe itself (no separate process, no OAuth) for dev/demo, then later point at a real remote URL for prod — same resource/view contract.
- `ione new-app <name>` CLI scaffold: emits an app skeleton (artifact `view_config` template + optional `resources/list` stub with `ione_view` examples + seed data + a README pointing at [building-on-ione.md](../playbooks/building-on-ione.md)). Turns the six-surface contract into fill-in-the-blanks.
- **Acceptance:** `ione new-app pinger` produces a runnable artifact-dashboard app that renders in a workspace with zero hand-written IONe code. **Effort:** ~2 days.

**ONR-003 — Demo auth profile.** A clean single-tenant login so a federal buyer can click through, distinct from the (largely unbuilt) brokered-identity path.

- Add `IONE_AUTH_MODE=demo-tenant`: one configured passcode / magic-link grants a scoped read-only session to a demo workspace. Not the OIDC broker (that is the S0–S5 identity-broker plan, separate); this is the minimal "show me the thing" gate that today only exists as hardcoded-local or full bypass.
- **Acceptance:** fresh browser → passcode → demo workspace, no other auth. **Effort:** ~1–1.5 days.

---

## Phase 3 — Geospatial parity + proof

**ONR-004 — No-peer map tab + static map overlays.**
- **Map tab gating (depends on ONR-001a):** update `TAB_REGISTRY` (`static/app.js`) to show the map tab when `panels.maps`/`panels.eventLayers > 0`, not only when `hasActivePeer`. Add a browser test for a no-peer artifact workspace with event layers.
- **Static overlays:** allow a map layer to reference a bundled/static overlay dir or a single-image overlay with `bounds` (image-overlay), served by IONe's static middleware or S3, so static geospatial demos (doi-ss-ping's GeoTIFF→PNG overlays) work without a tile server. Keep the no-proxy rule for true external tiles.
- **Acceptance:** a no-peer workspace shows its event-layer map tab; doi-ss-ping overlay renders from a baked asset. **Effort:** ~2–2.5 days.

**ONR-005 — Port doi-reclamation + measure.** The proof. Port the bridge-case app onto IONe and **time it end-to-end against the ~10-day standalone build.**

- doi-reclamation needs **chart + table + gauge**. Native artifact rendering produces line/aggregate panels only, so route the **gauge** either through `view_config.charts[]` (ONR-001a) once it lands, or through a thin MCP `resources/read` chart resource (peer path) — decide per the time-to-demo goal.
- Deliverable: doi-reclamation running inside IONe (chart + table + gauge), plus a one-page time-to-demo writeup. If IONe does **not** beat standalone, the on-ramp is not ready — feed the gaps back into ONR-001/001a/002. **Effort:** ~1–2 days once Phases 1–2 land.

---

## Sequencing & estimate

```
Phase 0: ONR-001a, ONR-008 (parallel) (~2d)   <- contract hardening BEFORE the clock
Phase 1: ONR-001                       (~3d)   <- unlock
Phase 2: ONR-002, ONR-003 (parallel)   (~3d)   <- scaffold + demo login
Phase 3: ONR-004, then ONR-005         (~3-4d) <- map tab + overlays, then the proof
CAU-*  : connector auth (parallel track, ~3d Tier-1) — see connector-auth-plan.md
-------------------------------------------------
Total ~11-12 working days incl. hardening. ONR-007 (notifications) re-enters after ONR-005.
```

ONR-001a/ONR-008 are Phase 0 — they touch disjoint areas (ingest/contract vs signal-creation) and gate the rest. ONR-002 and ONR-003 touch disjoint files (api/tooling vs auth) → parallelizable per the parallel-agents preflight. ONR-001 depends on ONR-001a's dedup migration + panels contract; ONR-004 depends on ONR-001a's `panels.maps` count. The CAU-* connector-auth track runs in parallel; its Tier-1 (CAU-001/002) is needed before any artifact pulled from a gated URL.

## Risks

- **Thesis drift.** Over-investing in single-app polish could pull IONe toward standalone-product shape (prohibited until Y3 gate). Mitigation: the guardrail above — every task must also speed peer onboarding; ONR-005 measures whether it does.
- **Parquet/Arrow dependency weight** in a Rust binary. Mitigation: feature-flag the artifact connector; CSV/GeoJSON are dependency-light and cover most demos.
- **Demo auth scope creep** into the real identity broker. Mitigation: ONR-003 is explicitly *not* OIDC; it is a demo gate. Hard-stop at passcode/magic-link.

## Definition of done

A new artifact-dashboard app renders in an IONe workspace in **< 1 day** with no hand-written IONe code (ONR-002 scaffold + ONR-001 ingest), a buyer can log into it cleanly (ONR-003), geospatial demos work without a tile server (ONR-004), and the doi-reclamation port beats its standalone build time (ONR-005). Then resume `notifications/*` (ONR-007).
