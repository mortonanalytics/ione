# Federated Catalog Search (Lexical-First)

**Status:** Reviewed (security + sql-architect + devil's advocate + technical-writer passes complete; PM/research carried from the prior research brief) — ready for `/implement`
**Layers:** `db`, `api`, `ui`
**Demand signals:** DICE §2.4 bounded-context ("a tool is exposed to an agent only when relevant to its task"; full proposal due 2026-08-25) · substrate thesis "surfaced upon relevance" · infrastructure backlog P3
**Builds on:** RBAC `tool_invoke` gating (`route_tool_call`, `auth::permission_grants`), the federation manifest lifecycle (`refresh_manifest_if_changed`), the panel UI pattern (probe-and-hide is explicitly **not** used here — see Slice 2 UI).
**Finding labels** in this doc are prefixed `FCS-` to disambiguate from the RBAC doc's own C-1/C-2.

---

## Decision (settled, not re-litigated here)

Relevance ranking is **lexical** — Postgres weighted full-text (`tsvector` + `websearch_to_tsquery` + `ts_rank_cd`) plus trigram fuzzy (`pg_trgm`). **No embeddings in v1.** Rationale, verified: tool/resource descriptions are terse technical text (20–80 tokens) over a small corpus (hundreds–low-thousands of rows/org) — the worst case for dense embeddings, the best case for lexical. The DARPA BAA (HR001126S0010) contains **zero** mentions of vector/semantic search; the abstract claims only "surfaced upon relevance," which lexical `ts_rank` satisfies literally. A `vector(768)` column is reserved **nullable and unpopulated** so a future hybrid reranking step is an additive change, not a migration — gated behind a *measured* lexical miss, never built speculatively.

**Empirically validated** (devil's advocate, run against the live DB): query "flood risk" ranked the hydrology tool #1 (0.96) and correctly excluded a financial "risk" tool — lexical AND-semantics avoids the exact cross-topic false positive vectors are prone to.

## Problem statement

IONe federates peer MCP servers; each peer's manifest carries tools (name + description + JSON input schema) and resources (uri + name + description + mime). Routing today is exact `prefix:name` — there is **no discovery**. An agent or operator cannot ask "what can surface flood-risk data?" and get the relevant capabilities ranked across all federated peers. At DICE scale (500–100K agents, many peers), handing an agent every federated tool floods its context and degrades selection; the §2.4 bounded-context claim requires the inverse — surface only the relevant, invokable subset. The mechanism that makes "surfaced upon relevance" real does not exist yet.

## Non-goals

- Embeddings / Ollama wiring — deferred to a hybrid v2 behind a measured lexical miss; the reserved column is the only v1 footprint.
- Search over data, documents, signals, or transcripts — this indexes **tool/resource metadata only**, not payloads.
- A cross-IONe-node shared/global index — each node indexes its own peers' manifests.
- A standalone enterprise-search product — Path 2 prohibits standalone-product positioning; this is the federation fabric's discovery layer.
- Fixing the pre-existing `tools/list` non-filtering gap (FCS-L1) — noted, with a shared helper that makes the later fix trivial, but not in scope.

---

## Feature slices

### Slice 1 — Catalog index + manifest-driven lifecycle (foundation)

Extract every peer tool/resource into a lexically-indexed, org-scoped catalog row, maintained off the existing manifest-refresh path.

- **DB:** **new migration `0043` (to be created)** adds `CREATE EXTENSION pg_trgm` (only `vector`+`pgcrypto` exist today). **New migration `0044` (to be created)** adds `peer_catalog_entries`: `id`, `org_id` (FK organizations), `peer_id` (FK peers `ON DELETE CASCADE`), `kind` (enum tool|resource), `namespaced_name` (the `prefix:name` used for invocation **and** RBAC glob matching — the join key to permission strings; canonical format `<peer.name>:<tool_name>`, where `peer.name` is the `peers.name` slug — the same format `route_tool_call` builds and `tool_invoke:<peer_slug>:*` grants use per `rbac.md` §Gate-to-permission map), `raw_name`, `description`, `sample_queries` (text[]), `schema_field_names` (text[], top-level JSON-schema property names extracted at index time), `content_hash` (SHA-256 of the indexed source, for delta reindex), reserved nullable `embedding vector(768)` (unused v1), `created_at`/`updated_at`, `UNIQUE(org_id, peer_id, namespaced_name)`. A **generated** `tsvector` column weights `setweight`: A=`raw_name`, B=`sample_queries`, C=`description`, D=`schema_field_names`. Indexes: GIN on the tsvector, GIN trgm on `(raw_name || ' ' || description)`, btree `(org_id, peer_id)`.
- **API:** none (internal lifecycle). The reindex hooks into `refresh_manifest_if_changed`: after the existing manifest hash check, for each manifest tool/resource compute the per-entry `content_hash`; **upsert** changed entries (`ON CONFLICT … DO UPDATE … WHERE content_hash <> EXCLUDED.content_hash` — no-op + no GIN churn when unchanged); **delete orphans** (entries whose `namespaced_name` vanished from the manifest). `org_id` is derived from `peers.org_id` (**not** the org-blind in-memory `peer_manifest_cache`, which is keyed on `peer_id` only and has no org dimension — populating from it would allow cross-org entries; see FCS-H2). Peer-supplied `description`/`sample_queries` pass through `sanitize_catalog_text` (**new**; wraps the existing `sanitize_slice_text` sentinel-stripper in `src/services/federation.rs`, additionally caps description at 512 chars) at index time (FCS-M2).
- **UI:** none.
- **Cross-reference:** `refresh_manifest_if_changed` → `CatalogRepo::{upsert_entry, delete_orphans, hashes_for_peer}` → `peer_catalog_entries`.

### Slice 2 — RBAC-pre-filtered relevance search (operator-facing: API + UI)

A workspace member searches federated capabilities and sees only what they can actually invoke, relevance-ranked.

- **DB:** no new schema. The ranked query: `WHERE org_id=$1 AND namespaced_name = ANY($invokable) AND (tsv @@ websearch_to_tsquery('english',$q) OR (raw_name||' '||description) % $q) ORDER BY (coalesce(ts_rank_cd(tsv, websearch_to_tsquery('english',$q)),0)*1.5 + similarity(raw_name||' '||description,$q)*0.5) DESC LIMIT $k`. `websearch_to_tsquery` only — `to_tsquery` is banned (injection, FCS-H3); all values bound params.
- **API:** `GET /api/v1/workspaces/:id/catalog-search?q=…&kind=tool|resource&limit=int`. **Pre-filter (FCS-C1):** the invokable set is computed by calling `effective_permissions(user_id, workspace_id)` (the resolver `require_permission` uses) to get the held grants, then filtering candidate `namespaced_name`s through the per-segment glob matcher `auth::permission_grants` — the exact code path `route_tool_call` traverses, never a SQL glob re-implementation, so search visibility exactly equals invocation capability. **Auth (FCS-C2):** reject when the caller is the unauthenticated default-user fallback (`is_authenticated()` false); requires session/SA + workspace-in-org (`ensure_workspace_in_org`, re-validated at query time per FCS-L2 — a stale session could carry a workspace the user was since removed from). Caps: `q` min length 2, `limit` clamp 1–50 (FCS-M3).
- **UI:** a **Catalog** tab in the workspace shell (always available to authenticated members — *not* probe-and-hide). **No permission beyond workspace membership gates browsing**: `peers:manage`, `audit:read`, and `tool_invoke` are explicitly **not** browse gates — they are enforced only at result-visibility level via the RBAC pre-filter, so results already self-filter to the caller's invokable set. Search box + ranked result list (peer, namespaced name, kind, description, sample queries). Peer-supplied strings rendered via `escapeHtml`/`textContent` only — reuse the `renderPeerBrowserItems` pattern, never `marked.parse` without sanitize (XSS, FCS-M1).
- **Cross-reference:** `CatalogPanel` → `GET …/catalog-search` → `CatalogService::search` (Rust pre-filter) → `CatalogRepo::search` → `peer_catalog_entries`.

### Slice 3 — `search_catalog` MCP tool (agent-facing; the DICE bounded-context surface)

An agent calls one MCP tool to retrieve only the task-relevant, invokable tools — the mechanism behind §2.4.

- **DB:** none (reuses Slice 2's query).
- **API:** a new MCP tool `search_catalog` (args: `query`, optional `kind`, optional `limit`) on the IONe MCP endpoint. Same Rust pre-filter + same auth rejection as Slice 2 (FCS-C1/FCS-C2); the MCP path must resolve a **real** `org_id` + permission set and reject the default-user fallback. Each result carries an explicit `untrusted_content: true` flag and a sanitized, length-capped `description` (FCS-M2) so the agent's system prompt can treat surfaced peer metadata as untrusted (prompt-injection defense).
- **UI:** none (agent-facing).
- **Cross-reference:** MCP `tools/call search_catalog` → `CatalogService::search` (shared with Slice 2) → `peer_catalog_entries`.

---

## API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/catalog-search` | GET | `?q=string(len≥2)&kind=enum(tool,resource)?&limit=int(1..50)` | `{ items: [{ peer_id:UUID, peer_name:string, namespaced_name:string, kind:enum, description:string, sample_queries:string[], score:float }] }` | 400, 401, 403, 404 | Session/SA + workspace-in-org + **authenticated** (default-user fallback rejected) |
| MCP tool `search_catalog` | tools/call | `{ query:string(len≥2), kind?:enum(tool,resource), limit?:int(1..50) }` | `{ results: [{ namespaced_name, kind, description, sample_queries, untrusted_content:true, score }] }` | JSON-RPC error on unauth / bad args | MCP session w/ real org_id + permission set; default-user fallback rejected |

**Contract rules:** results are **pre-filtered** to the caller's `tool_invoke`-invokable set (computed by `effective_permissions` then glob-filtered via `auth::permission_grants`, the exact path `route_tool_call` uses) — a result the caller cannot invoke must never appear (info-disclosure). `q` shorter than 2 chars → 400. `limit` clamped to ≤50. The query string is passed only to `websearch_to_tsquery` (never `to_tsquery`). Peer-supplied `description`/`sample_queries` are sanitized at index time and rendered escaped at the UI; the MCP response flags them `untrusted_content`. Empty/whitespace `q` → 400 (no full-catalog dump). **The MCP `search_catalog` response omits `peer_id`/`peer_name` intentionally** — an agent needs only `namespaced_name` to invoke a tool, whereas the UI needs both for display. AC-10's "same invokable, ranked set" asserts set identity + score ordering, not response-shape identity.

## Wiring dependency graph

```mermaid
graph LR
  CatalogPanel["Catalog panel (workspace shell)"] --> SearchEP["GET …/catalog-search"]
  SearchEP --> Svc["CatalogService::search"]
  AgentMCP["agent → MCP tools/call search_catalog"] --> Svc
  Svc --> PreFilter["invokable set: effective_permissions(user, workspace) then glob-filter via auth::permission_grants (same path as route_tool_call)"]
  PreFilter --> Repo["CatalogRepo::search (websearch_to_tsquery + trgm, ANY(invokable))"]
  Repo --> CAT[("peer_catalog_entries (0044) + GIN tsv, GIN trgm")]
  subgraph Index lifecycle (Slice 1)
    Refresh["refresh_manifest_if_changed"] --> Sanitize["sanitize_catalog_text (strip sentinels, cap 512)"]
    Sanitize --> Upsert["CatalogRepo::upsert_entry / delete_orphans (org_id from peers.org_id)"]
    Upsert --> CAT
  end
```

## Tradeoffs

| Decision | Alternative | Why this wins |
|---|---|---|
| Lexical FTS + trgm | Dense embeddings | Terse text + tiny corpus is lexical's strong case and embeddings' weak case; empirically lexical excluded the cross-topic false positive vectors make; no model/throughput/re-embed dependency. Reserved column keeps hybrid a later additive option. |
| Reserved nullable `vector(768)`, unused | Add the column later when hybrid is built | Adding a nullable column now avoids a future table migration; costs nothing in v1 (never read/written). |
| Pre-filter computed in Rust via `permission_grants` | Replicate `tool_invoke` glob matching in SQL | SQL glob would diverge from `route_tool_call`'s Rust matcher → either a leak (search shows more than you can invoke) or confusing UX (less). Identical code path guarantees visibility == capability. |
| Generated `tsvector` column | App-built or trigger-maintained vector | Generated-stored can't be forgotten on write, no cross-table join, and the write path is background (manifest refresh), so GIN reindex cost is fine. |
| Catalog tab always-visible (no manage-permission gate) | Probe-and-hide like roles/policies/tokens | Results self-filter to the caller's invokable set, so there's nothing to hide behind a permission; gating browse would add friction with no security benefit. |
| Index `description` + `sample_queries` + schema fields, weighted | Index description only | Terse descriptions alone underperform; `sample_queries` (peer-supplied NL queries) give query-like surface area — the single biggest ranking-quality lever (helps lexical, not just vectors). |

## Acceptance criteria

Each maps to an integration test (new `catalog_search_integration.rs`; `spawn_app` per `tests/audit_export_integration.rs`).

1. **Index from manifest:** Given a peer whose manifest has 3 tools, when its manifest refreshes, then `peer_catalog_entries` has 3 `tool` rows for that `(org_id, peer_id)` with populated `tsv`, and each `namespaced_name` equals the `prefix:name` used by `route_tool_call`.
2. **Delta reindex (no churn):** Given an indexed peer, when the manifest refreshes with **unchanged** tools, then no row's `updated_at` advances (content_hash guard); when one tool's description changes, then exactly that row's `updated_at` advances.
3. **Orphan delete:** Given an indexed peer, when a tool is removed from its manifest and it refreshes, then that tool's catalog row is gone and the others remain.
4. **Relevance ranking (the lexical-precision property):** Given a hydrology "flood" tool and a finance "risk" tool both seeded and invokable by the caller, when `catalog-search?q=flood risk` is called, then status 200, the hydrology tool **appears** in results, and the finance tool **does not** (websearch AND-semantics requires both "flood"&"risk"; only the hydrology entry carries "flood"). This is the empirically-verified outcome — no disjunction.
5. **RBAC pre-filter (the core security property):** Given a caller holding `tool_invoke:weatherpeer:*` but **not** `tool_invoke:finpeer:*`, when they search a query matching a `finpeer` tool, then that tool is **absent** from results; a caller holding neither gets zero results for it. The invokable set used equals what `route_tool_call` would permit.
6. **Unauthenticated rejection:** Given the unauthenticated default-user fallback context (no session, no SA), when `catalog-search` or `search_catalog` is called, then 403 / JSON-RPC error — never the default user's permissions.
7. **Injection-safe query:** Given `q = "' | (select 1) &"`, when searched, then status 200 with no error — the 200 is the observable proxy for `websearch_to_tsquery` compliance (a `to_tsquery`-based impl would parse-error to 500 on this input).
8a. **Short-query reject:** Given `q="a"` (length 1), when searched, then 400.
8b. **Limit clamp:** Given `limit=500`, when searched, then at most 50 items are returned.
9. **Sanitized peer content:** Given a peer tool whose description contains an IONE slice sentinel and `<img onerror=…>`, when indexed, then the stored description has the sentinel stripped and is ≤512 chars; when returned via `search_catalog`, the result carries `untrusted_content:true`.
10. **MCP tool parity:** Given identical query + caller, when invoked via `GET catalog-search` and via the `search_catalog` MCP tool, then both return the same invokable, ranked set (shared service).
11. **UI escaping:** Given a peer description containing `<script>`, when the Catalog panel renders it, then it appears as literal text (escaped), not executed (verified in the Playwright spec via the rendered text content).

## Open questions

1. **Ranking quality without `sample_queries`.** The empirical win leaned on the peer shipping `sample_queries` containing query vocabulary. If a peer ships only a terse description (no sample_queries) and the query uses different words (e.g. description "inundation outlook", query "flood"), lexical misses — this is precisely the documented **hybrid-v2 trigger**. v1 ships lexical and measures; if real peers don't supply sample_queries and misses are common, that's the evidence to build hybrid. Not a v1 blocker.
2. **`tools/list` unification (FCS-L1).** The pre-existing `tools/list` MCP path returns all workspace tools with no `tool_invoke` filter. Out of scope here, but Slice 2/3's `invokable_tools_for_caller` helper should be written so a later fast-follow can apply it to `tools/list` trivially. Named, not dropped.
3. **RLS dormancy (FCS-H1).** `peer_catalog_entries` gets an org-isolation RLS policy for consistency, but like the existing tables it is inert (`app.current_org_id` is never set) — the application `WHERE org_id=$1` is the real guard. Stated honestly; fixing RLS globally is a separate cross-cutting task.

## Commercial linkage

This is the "surfaced upon relevance" mechanism the substrate thesis names and the §2.4 bounded-context claim the DICE proposal must demonstrate — relevance-ranked, RBAC-scoped tool exposure that keeps agent context bounded as peer count grows, shown as a working `search_catalog` tool rather than a diagram. For Path 2, it's the federation fabric's discovery layer (an operator/agent finds capabilities across federated domain apps without enumerating manifests) — never positioned as standalone search.

## Requirements impact

Create `md/requirements/active/federated-catalog-search.md`: the `peer_catalog_entries` shape + lifecycle (manifest-driven, content-hash delta, orphan delete), the two search contracts (REST + MCP tool), the **pre-filter-equals-tool_invoke** invariant, the `websearch_to_tsquery`-only / sanitize-peer-content rules, and the lexical-now / hybrid-deferred decision with its trigger. The RBAC requirements doc's `tool_invoke` section gains a note that catalog search reuses the same `permission_grants` matcher for visibility.

---

## Devil's Advocate

**1. What assumption, if wrong, invalidates the design?**
That lexical full-text ranking over short tool metadata returns *useful* relevance — i.e. that there's enough indexable signal (name + sample_queries + description + schema fields) for `ts_rank_cd`/trgm to put the right tool on top and keep wrong-topic tools off it. If lexical ranking is noise on this corpus, the whole "surfaced upon relevance" surface is worthless regardless of how cleanly it's built.

**2. Verified against live state?**
Ran the actual ranking against the live pg16 DB (`localhost:5433`): built the weighted tsvector + trgm over four representative tools and queried "flood risk." **Result: VERIFIED ✓** — the hydrology tool scored 0.96 and ranked #1; the finance "risk" tool was **excluded entirely** because `websearch_to_tsquery` ANDs "flood"&"risk" and "flood" appears only in the hydrology entry. This both confirms the mechanism works in the target engine *and* demonstrates lexical's AND-semantics avoids the cross-topic false positive that motivated rejecting vectors. The one boundary it exposed (the win relied on `sample_queries` carrying "flood risk") is captured as Open Question 1 and is the explicit hybrid-v2 trigger — a known limit, not a refutation.

**3. Simplest alternative that avoids the biggest risk?**
Skip the catalog table entirely; have `search_catalog` filter the in-memory `peer_manifest_cache` with substring/`contains` matching at query time — no migration, no reindex lifecycle. Rejected because: (a) substring match has no relevance ranking (the whole point), (b) it offers no RBAC pre-filter integration without re-deriving the invokable set per call anyway, and (c) the cache is org-blind (FCS-H2) so query-time filtering would re-introduce the cross-org risk the persisted org-scoped table closes. The table + GIN indexes are the minimum that delivers *ranked, isolated, pre-filtered* results. The lifecycle cost is bounded — it piggybacks on the existing `refresh_manifest_if_changed` event and the content-hash guard makes steady-state refresh a no-op.

**4. Structural completeness checklist**
- [x] Every UI call (Catalog panel search) is in the API contract table.
- [x] Every endpoint maps to a repo method (`CatalogRepo::search`; lifecycle uses `upsert_entry`/`delete_orphans`/`hashes_for_peer`); the MCP tool shares `CatalogService::search`.
- [x] New fields (`namespaced_name`, `sample_queries`, `score`, etc.) appear across DB (0044), API (contract + result shape), and UI (result list). Reserved `embedding` is DB-only by design (stated unused).
- [x] Each acceptance criterion names an endpoint/MCP tool + expected outcome (1–11).
- [x] Wiring graph is unbroken UI/agent → service → repo → table, plus the index-lifecycle subgraph.
- [x] Integration scenarios cover one full path per slice: AC-1/2/3 (Slice 1 lifecycle), AC-4/5/7/8a/8b/11 (Slice 2 search+UI), AC-6/10 (Slice 3 MCP+auth), AC-9 spans Slice 1 (index-time sanitize: stored description stripped + ≤512 chars) and Slice 3 (`untrusted_content` flag) — test as two sub-assertions.
