# Federated Catalog Search — Implementation Plan

**Design doc:** `md/design/federated-catalog-search.md`
**Shape:** medium-large (db + api + ui + MCP tool, ~15 files; 3 sequential vertical-slice phases, no task manifest, no contract file)
**Stack:** Rust/Axum + Postgres (sqlx, pgvector/pgvector:pg16) + static HTML/JS UI. Integration tests `#[ignore]`-gated, run `--ignored --test-threads=1` with `IONE_SKIP_LIVE=1`; e2e server boot needs `IONE_TOKEN_KEY` + `IONE_WEBHOOK_SECRET_KEY`. **CI runs `cargo fmt --check` — run it locally before pushing.**

## Dependencies

None. `pg_trgm` is a Postgres extension (migration), not a crate. `sha2` (content_hash), `pgvector` (reserved column type), sqlx JSONB/array all present.

## Resolved-at-plan-time facts (verified against working tree)

- `refresh_manifest_if_changed` ([federation.rs:217](src/services/federation.rs#L217)) has `peer` (carries `org_id`, `name`) and `new_manifest` in scope — the reindex hook lands here after the hash check.
- `PeerManifest` ([federation.rs:26](src/services/federation.rs#L26)): `tools: Vec<Value>`, `resources: Vec<Value>` (raw MCP JSON objects — extract `name`/`description`/`inputSchema` for tools, `name`/`description`/`uri`/`mimeType` for resources). `namespaced_tools_from_manifest(peer, manifest)` ([federation.rs:703](src/services/federation.rs#L703)) shows the `peer.name:raw_name` namespacing to reuse.
- `RoleRepo::effective_permissions(user_id, workspace_id)` ([role_repo.rs:96](src/repos/role_repo.rs#L96)) → held grant set; `auth::permission_grants(held: &HashSet<String>, needed: &str) -> bool` ([auth.rs:370](src/auth.rs#L370)) is the glob matcher `route_tool_call` uses. Reuse both verbatim for the pre-filter (FCS-C1).
- `sanitize_slice_text` exists in `src/services/federation.rs` (~929) — `sanitize_catalog_text` wraps it + 512-char cap.
- MCP built-in tools: JSON list in [mcp_server.rs:99-160](src/mcp_server.rs#L99) + dispatch match ([mcp_server.rs:372](src/mcp_server.rs#L372)). Add a `search_catalog` entry + arm + handler.
- `sample_queries` source: peer **context slices** (not the manifest). Pull from the peer slice cache/`fetch_slice` if present; **empty array if the peer ships no slice** (Open Question 1 — quality impact, not a blocker).
- Migration numbering: next free is **0043** (0042 = provisioning constraints).
- UI shell `tab-*`/`panel-*`/`switchTab()`; `renderPeerBrowserItems` ([app.js ~3418](static/app.js)) is the escape-safe peer-content render pattern to mirror (FCS-M1).

## Phases

### Phase 1 — Catalog index + manifest-driven lifecycle (design Slice 1; foundation)

**Goal:** every peer tool/resource is a lexically-indexed, org-scoped catalog row, maintained off `refresh_manifest_if_changed`.

**Files:**
- `migrations/0043_catalog_extensions.sql` — **create.** `CREATE EXTENSION IF NOT EXISTS pg_trgm;`
- `migrations/0044_peer_catalog_entries.sql` — **create.**
```sql
CREATE TYPE catalog_entry_kind AS ENUM ('tool','resource');
CREATE TABLE peer_catalog_entries (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
  peer_id UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
  kind catalog_entry_kind NOT NULL,
  namespaced_name TEXT NOT NULL,         -- '<peer.name>:<raw_name>'
  raw_name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  sample_queries TEXT[] NOT NULL DEFAULT '{}',
  schema_field_names TEXT[] NOT NULL DEFAULT '{}',
  content_hash TEXT NOT NULL,
  embedding vector(768),                 -- reserved, unused v1
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  tsv tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(raw_name,'')),'A') ||
    setweight(to_tsvector('english', coalesce(array_to_string(sample_queries,' '),'')),'B') ||
    setweight(to_tsvector('english', coalesce(description,'')),'C') ||
    setweight(to_tsvector('english', coalesce(array_to_string(schema_field_names,' '),'')),'D')
  ) STORED,
  CONSTRAINT pce_unique_entry UNIQUE (org_id, peer_id, namespaced_name)
);
CREATE INDEX pce_tsv_gin ON peer_catalog_entries USING gin (tsv);
CREATE INDEX pce_trgm_gin ON peer_catalog_entries USING gin ((raw_name || ' ' || description) gin_trgm_ops);
CREATE INDEX pce_org_peer ON peer_catalog_entries (org_id, peer_id);
-- BEFORE UPDATE touch updated_at; org-isolation RLS policy per existing (inert) pattern.
```
- `src/models/catalog_entry.rs` — **create.** `CatalogEntry` (FromRow; `tsv`/`embedding` not deserialized), `CatalogEntryKind` enum.
- `src/models/mod.rs` — export.
- `src/repos/catalog_repo.rs` — **create.** `upsert_entry(...)` (ON CONFLICT … WHERE content_hash<>EXCLUDED), `delete_orphans(org_id, peer_id, surviving_names: &[String])`, `hashes_for_peer(org_id, peer_id) -> Vec<(String,String)>`, plus `search(...)` (Phase 2).
- `src/repos/mod.rs` — export `CatalogRepo`.
- `src/services/federation.rs` — add `sanitize_catalog_text(input) -> String` (wrap `sanitize_slice_text`, truncate 512); add `reindex_peer_catalog(state, peer, manifest)` that extracts each tool/resource → `(namespaced_name, raw_name, description, schema_field_names, sample_queries)`, computes `content_hash = sha256(raw_name|description|sample_queries|schema_fields)`, upserts, then `delete_orphans` for vanished names; call it at the end of `refresh_manifest_if_changed` using `peer.org_id`. `sample_queries` pulled from the peer slice if cached, else `[]`.
- `tests/catalog_search_integration.rs` — **create** (`_integration` suffix; `spawn_app` per `tests/audit_export_integration.rs`). Phase-1 tests: AC-1 (manifest→3 rows, namespaced_name matches), AC-2 (delta: unchanged→no updated_at bump, one change→one bump), AC-3 (orphan delete), AC-9a (sentinel stripped + ≤512 chars on stored description).

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test catalog_search_integration phase1 -- --ignored --test-threads=1` + `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`.
**Acceptance:** AC-1/2/3/9a green as named tests.

---

### Phase 2 — RBAC-pre-filtered search: service + REST + UI (design Slice 2)

**Goal:** a workspace member searches federated capabilities and sees only their invokable set, relevance-ranked.

**Files:**
- `src/repos/catalog_repo.rs` — add `search(org_id, invokable: &[String], q: &str, kind: Option<CatalogEntryKind>, limit: i64) -> Vec<CatalogSearchRow>`:
```sql
SELECT id, peer_id, kind, namespaced_name, raw_name, description, sample_queries,
  (coalesce(ts_rank_cd(tsv, websearch_to_tsquery('english',$3)),0)*1.5
   + similarity(raw_name||' '||description,$3)*0.5) AS score
FROM peer_catalog_entries
WHERE org_id=$1 AND namespaced_name = ANY($2)
  AND ($5::catalog_entry_kind IS NULL OR kind=$5)
  AND (tsv @@ websearch_to_tsquery('english',$3) OR (raw_name||' '||description) % $3)
ORDER BY score DESC LIMIT $4
```
  (`websearch_to_tsquery` only; all bound params.)
- `src/services/catalog.rs` — **create.** `CatalogService::search(state, workspace_id, auth, q, kind, limit)`: (1) reject unauthenticated (`auth.is_authenticated()` — add this predicate to `AuthContext`: false when `user_id == default_user_id && !is_oidc && !is_service_account`) → 403; (2) `ensure_workspace_in_org` (re-validate, FCS-L2); (3) clamp limit 1–50, reject `q.trim().len() < 2` → 400; (4) compute invokable set via `invokable_tools_for_caller(state, workspace_id, auth)` — `effective_permissions` → for each catalog `namespaced_name` in org, keep those where `permission_grants(&held, &format!("tool_invoke:{}", namespaced_name))` (FCS-C1); (5) `CatalogRepo::search`. Returns rows + peer_name (join/lookup).
- `src/auth.rs` — add `AuthContext::is_authenticated(&self, default_user_id: Uuid) -> bool`.
- `src/routes/catalog.rs` — **create.** `GET /api/v1/workspaces/:id/catalog-search` handler → `CatalogService::search` → `{ items: [...] }` (REST shape includes `peer_id`+`peer_name`).
- `src/routes/mod.rs` — mount the route.
- `static/index.html` — `tab-catalog` + `panel-catalog`: search input + ranked result list. **Always-visible tab** (no probe-and-hide).
- `static/app.js` — `loadCatalog()` / search handler; render results with `escapeHtml`/`textContent` (mirror `renderPeerBrowserItems`, FCS-M1) — never `marked.parse`.
- `static/style.css` — catalog panel styles.
- `tests/catalog_search_integration.rs` — Phase-2 tests: AC-4 (flood/finance ranking — both seeded, hydro appears, finance absent), AC-5 (RBAC pre-filter: caller lacking `tool_invoke:finpeer:*` never sees finpeer tools), AC-7 (injection string → 200), AC-8a (`q="a"`→400), AC-8b (`limit=500`→≤50).
- `tests/e2e/catalog-panel.spec.ts` — **create.** AC-11 (peer `<script>` description renders as escaped text).

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test catalog_search_integration phase2 -- --ignored --test-threads=1` + fmt + clippy + `npx playwright test tests/e2e/catalog-panel.spec.ts`.
**Acceptance:** AC-4/5/7/8a/8b green; AC-11 in Playwright.

---

### Phase 3 — `search_catalog` MCP tool (design Slice 3; DICE bounded-context surface)

**Goal:** an agent retrieves only task-relevant, invokable tools via one MCP tool.

**Files:**
- `src/mcp_server.rs` — add a `search_catalog` entry to the built-in tools JSON list (~line 99), a dispatch arm (~line 372), and `tool_search_catalog(args, auth, state)` calling the **shared** `CatalogService::search`. The MCP path must resolve a real `org_id` + permission set and **reject the default-user fallback** (FCS-C2) — return JSON-RPC error if `!auth.is_authenticated()`. Response shape: `{ results: [{ namespaced_name, kind, description, sample_queries, untrusted_content: true, score }] }` — omits `peer_id`/`peer_name` by design; `description` already sanitized at index time.
- `tests/catalog_search_integration.rs` — Phase-3 tests: AC-6 (default-user/unauth MCP call → error), AC-10 (REST and MCP return the same ranked invokable set for identical query+caller), AC-9b (`untrusted_content:true` on each result).

**Gate:** `IONE_SKIP_LIVE=1 cargo test --test catalog_search_integration -- --ignored --test-threads=1` (full file) + fmt + clippy.
**Acceptance:** AC-6/9b/10 green; full suite green together.

---

## Acceptance-criteria → phase map (self-review, step 7)

| Design AC | Phase | Gate test |
|---|---|---|
| 1 (index from manifest) | 1 | `catalog…::phase1_index_from_manifest` |
| 2 (delta reindex) | 1 | `…::phase1_delta_no_churn` |
| 3 (orphan delete) | 1 | `…::phase1_orphan_delete` |
| 9a (index-time sanitize) | 1 | `…::phase1_sanitize_stored` |
| 4 (lexical ranking precision) | 2 | `…::phase2_flood_ranks_finance_absent` |
| 5 (RBAC pre-filter) | 2 | `…::phase2_prefilter_hides_uninvokable` |
| 7 (injection-safe) | 2 | `…::phase2_injection_string_200` |
| 8a/8b (caps) | 2 | `…::phase2_short_query_400`, `…::phase2_limit_clamp` |
| 11 (UI escaping) | 2 | `catalog-panel.spec.ts` |
| 6 (unauth rejection) | 3 | `…::phase3_unauth_mcp_rejected` |
| 10 (REST/MCP parity) | 3 | `…::phase3_rest_mcp_parity` |
| 9b (untrusted_content flag) | 3 | `…::phase3_untrusted_flag` |

Every design AC maps to a phase gate. Cited files exist except those marked **create**. Phases are vertical slices (Phase 1 is the index foundation + its lifecycle tests; 2 and 3 each ship a full search surface). Gates are concrete commands. Sequential — no manifest/contract file.

## Notes for /code-the-plan

- **Run `cargo fmt` before each push** — the CI fmt gate bit three prior merges. Add `cargo fmt --check` to every phase gate (done above).
- **Compile at every boundary:** the `AuthContext::is_authenticated` addition and the `CatalogService` shared by Phases 2 and 3 — Phase 2 builds the service; Phase 3 only adds the MCP entry/arm/handler, no service change.
- **`namespaced_name` must equal what `route_tool_call` builds** (`<peer.name>:<raw_name>`) or the RBAC pre-filter silently returns nothing — assert this in AC-1.
- **`sample_queries` is best-effort** from the peer slice; empty if absent. Don't block indexing on a missing slice.
- **Environment preflight:** `pg_isready -h localhost -p 5433`; integration `IONE_SKIP_LIVE=1 … --ignored --test-threads=1`; e2e server boot needs `IONE_TOKEN_KEY`+`IONE_WEBHOOK_SECRET_KEY`.
- **Branch hygiene:** start from clean `main` (now includes the four prior features + UI polish).
- Mark the backlog P3 catalog item **Partial — pending walkthrough** when code-complete.
