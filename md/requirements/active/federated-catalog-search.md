# Requirements — Federated Catalog Search (Lexical-First)

**Source design:** `md/design/federated-catalog-search.md`
**Plan:** `md/plans/federated-catalog-search-plan.md`
**Status:** code-complete on `feature/federated-catalog-search` (Partial — pending founder walkthrough)

| Surface | Ships in |
|---|---|
| `peer_catalog_entries` index + manifest-driven lifecycle | Phase 1 |
| `GET /api/v1/workspaces/:id/catalog-search` + Catalog panel | Phase 2 |
| `search_catalog` MCP tool | Phase 3 |

## Catalog index (`peer_catalog_entries`, migrations 0043–0044)

- One org-scoped row per peer tool/resource: `org_id` (FK organizations), `peer_id` (FK peers `ON DELETE CASCADE`), `kind` (`catalog_entry_kind` enum `tool|resource`), `namespaced_name`, `raw_name`, `description`, `sample_queries text[]`, `schema_field_names text[]`, `content_hash`, reserved nullable `embedding vector(768)` (unused in v1), `created_at`/`updated_at`, `UNIQUE(org_id, peer_id, namespaced_name)`.
- `namespaced_name` is the **invocation form** `<peer.tool_prefix>:<raw_name>` — the exact string `route_tool_call` splits on (`namespaced_tools_from_manifest`). It is **not** the permission string; see the pre-filter invariant below.
- Generated `tsvector` (weighted: A=`raw_name`, B=`sample_queries`, C=`description`, D=`schema_field_names`) via the `pce_array_to_text` IMMUTABLE wrapper (plain `array_to_string` is STABLE and a generated column forbids it). Indexes: GIN on tsv, GIN trgm on `(raw_name || ' ' || description)`, btree `(org_id, peer_id)`. Inert org-isolation RLS (FCS-H1; application `WHERE org_id=$1` is the real guard).
- **Lifecycle:** hooked into `refresh_manifest_if_changed`. After the manifest hash check, `reindex_peer_catalog` upserts each tool/resource (`ON CONFLICT … DO UPDATE … WHERE content_hash <> EXCLUDED.content_hash` — no row write / no GIN churn / no `updated_at` bump when unchanged) and deletes orphans whose `namespaced_name` vanished. `org_id` derives from `peers.org_id`, **never** the org-blind `peer_manifest_cache` (FCS-H2).
- Peer-supplied `description`/`sample_queries` pass through `sanitize_catalog_text` at index time: strips slice sentinels (reuses `sanitize_slice_text`) and caps the description at 512 characters (FCS-M2). `sample_queries` are best-effort from the cached peer slice (`body.sample_queries[<raw_name>]`); empty when the peer ships no slice (Open Question 1 — quality, not a blocker).

## API contracts

| Endpoint | Method | Request schema | Response schema | Error codes | Auth |
|---|---|---|---|---|---|
| `/api/v1/workspaces/:id/catalog-search` | GET | `?q=string(len≥2)&kind=enum(tool,resource)?&limit=int(1..50)` | `{ items: [{ peer_id:UUID, peer_name:string, namespaced_name:string, kind:enum, description:string, sample_queries:string[], score:float }] }` | 400, 401, 403, 404 | Session/SA + workspace-in-org + **authenticated** (default-user fallback rejected) |
| MCP tool `search_catalog` | tools/call | `{ workspace_id:UUID, query:string(len≥2), kind?:enum(tool,resource), limit?:int(1..50) }` | `{ results: [{ namespaced_name, kind, description, sample_queries, untrusted_content:true, score }] }` | JSON-RPC error on unauth / bad args | MCP session w/ real org_id + permission set; default-user fallback rejected |

**Contract rules:**
- **Pre-filter invariant (FCS-C1) — visibility == invocation capability.** Results are pre-filtered to the caller's invokable set, computed by `effective_permissions(user_id, workspace_id)` then, **per entry**, glob-matched via `auth::permission_grants` against `tool_invoke:<peer.name>:<raw_name>` — byte-for-byte the string `route_tool_call` builds (note: keyed on `peers.name`, **not** `tool_prefix`; the two diverge for non-slug names). `admin` short-circuits, matching `route_tool_call`. A result the caller cannot invoke must never appear.
- **Auth (FCS-C2).** `AuthContext::is_authenticated(default_user_id)` is false for the unauthenticated default-user fallback (`user_id == default_user_id && !is_oidc && !is_service_account`); both surfaces reject it (REST 403, MCP JSON-RPC error) and never serve the default user's permissions.
- **Workspace-in-org re-validated at query time** (`ensure_workspace_in_org`, FCS-L2).
- **Query safety (FCS-H3).** `q` is passed only to `websearch_to_tsquery('english', …)` (never `to_tsquery`); all values are bound params. `q.trim()` shorter than 2 chars → 400. `limit` clamped 1–50.
- **Untrusted peer content.** `description`/`sample_queries` are sanitized at index time and rendered escaped in the UI (`escapeHtml` only — never `marked.parse`, FCS-M1); the MCP response flags every result `untrusted_content:true`.
- **MCP response omits `peer_id`/`peer_name` by design** — an agent needs only `namespaced_name` to invoke. REST/MCP parity (AC-10) asserts set identity + score ordering, not response-shape identity.
- **`workspace_id` is required on the MCP tool** (deviation from the design's contract table, which omitted it): the invokable set is workspace-scoped, so the tool cannot resolve permissions without it.

## Ranking

Lexical-only in v1: `ts_rank_cd(tsv, websearch_to_tsquery('english', q)) * 1.5 + similarity(raw_name||' '||description, q) * 0.5`, descending. `websearch` AND-semantics is load-bearing (e.g. "flood risk" excludes a finance "risk" tool that lacks "flood"). The reserved `vector(768)` column is the only v1 footprint of the deferred hybrid reranker; **building it is gated behind a measured lexical miss** (Open Question 1), never speculative.

## Cross-reference

The RBAC `tool_invoke` model (`md/requirements/active/rbac.md`) is reused verbatim for catalog visibility — the same `permission_grants` matcher and `effective_permissions` resolver that `route_tool_call` uses. The shared `invokable_names` helper is written so a later fast-follow can apply the same filter to the unfiltered `tools/list` MCP path (FCS-L1, out of scope here).
