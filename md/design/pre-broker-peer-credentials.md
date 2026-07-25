# Pre-broker peer credentials

**Status:** implemented (issue #19). **Superseded by:** brokered identity (#12).

IONe needs authenticated outbound access to a peer before the identity broker
exists. This is that mode: a static bearer credential IONe holds per
`(workspace, peer)`, encrypted at rest, presented on outbound MCP requests made
in that workspace's scope.

This is explicitly a **pre-broker** mode. #12 replaces where the bearer value
comes from; it does not change what the peer sees.

## Peer-side contract (unchanged by #12)

A peer receives exactly one thing from IONe in both modes:

```
Authorization: Bearer <credential>
```

Nothing distinguishes pre-broker from brokered on the wire — no extra header, no
different scheme, no marker in the value. A peer that accepts IONe's bearer today
accepts it unchanged after the broker lands, and needs no rebuild.

## Storage

`workspace_peer_credentials` (migration `0047_workspace_peer_credentials.sql`),
`UNIQUE (workspace_id, peer_id)`:

| column                  | note                                                          |
| ----------------------- | ------------------------------------------------------------- |
| `org_id`                | derived by the `wpc_check_same_org` trigger; RLS isolation key |
| `workspace_id`          | `ON DELETE CASCADE`                                           |
| `peer_id`               | `ON DELETE CASCADE`                                           |
| `credential_ciphertext` | `token_crypto::encrypt_versioned` under `IONE_TOKEN_KEY`      |
| `created_by`            | issuing user, `NULL` for service-account callers               |
| `created_at`            |                                                               |
| `rotated_at`            | `NULL` until first rotation                                    |

**Why a separate table rather than a column on `workspace_peer_bindings`.**
Binding rows are serialized wholesale into API responses (`routes/bindings.rs`
returns `serde_json::to_value(binding)`), so a ciphertext column there would ride
along on every binding read and every future binding query would have to remember
to exclude it. Credential lifetime is also independent of binding status — a
binding may sit `pending` or `conflict` while the credential stays valid.

**Encryption** uses the versioned envelope (`0x01 || nonce || AES-256-GCM`), the
same one `broker_credentials` uses, not the legacy un-versioned `encrypt_token`.
`peers.webhook_secret_ciphertext` is unrelated: that is the *inbound* webhook HMAC
secret and is never reused for outbound auth.

## Rotation is a config operation

`PUT /api/v1/workspaces/:id/peers/:peerId/credential` rewrites
`credential_ciphertext` on the existing row and stamps `rotated_at`. No schema
change, no new row, no migration. The next outbound request presents the new
value; the old value is unrecoverable.

The plaintext is returned **exactly once**, in the `PUT` response. No later read
returns it: `GET` and the workspace list return `models::WorkspacePeerCredential`,
which has no secret field at all, so echoing it is structurally impossible.
Audit rows carry `{workspace_id, peer_id}` only.

## API

All four are gated on workspace-scoped `peers:manage`, the same grant the binding
routes use. A workspace or peer outside the caller's org 404s before the
permission check runs, so cross-org probing reveals nothing.

| method   | path                                                  | returns                            |
| -------- | ----------------------------------------------------- | ---------------------------------- |
| `PUT`    | `/api/v1/workspaces/:id/peers/:peerId/credential`     | 201 create / 200 rotate, plaintext once |
| `GET`    | `/api/v1/workspaces/:id/peers/:peerId/credential`     | metadata, 404 if unset             |
| `DELETE` | `/api/v1/workspaces/:id/peers/:peerId/credential`     | 204                                |
| `GET`    | `/api/v1/workspaces/:id/peer-credentials`             | metadata list                      |

`PUT` body: `{"credential": "<peer-issued key>"}`. Omit `credential` and IONe
generates a 256-bit URL-safe value instead — useful when the operator administers
both ends (IONe↔IONe federation) and needs a value to install on the peer.

Audit verbs: `peer_credential.created`, `peer_credential.rotated`,
`peer_credential.deleted`, object kind `peer_credential`.

## Precedence

Outbound bearer resolution, highest first. The canonical statement lives on the
`services::peer_tokens::resolve_access_token` doc comment; #12 added tier 1 on
top of the three tiers this design shipped with:

1. the brokered delegated token for the request's workspace scope (#12),
2. the peer's brokered OAuth access token (`peers.access_token_ciphertext`),
3. the pre-broker credential for the request's workspace scope (#19),
4. the process-global `IONE_OAUTH_STATIC_BEARER` env fallback.

**OAuth outranks the static credential deliberately.** When a peer gains a
brokered token, outbound auth switches on the very next request with no flag day
and no operator action; the now-dormant static credential can be deleted
whenever convenient. The reverse ordering would require every operator to delete
credentials in lockstep with the broker rollout.

In the `mcp_client` connector the legacy `bearer_token` literal in connector
config stands in for tier 4 rather than for tier 2: the delegated token, the
peer-global OAuth token, and the per-(workspace, peer) credential all outrank
it, so rotating any of them through the API takes effect without rewriting
connector rows.

**A 401 never downgrades the tier.** The refresh-and-retry runs only when the
rejected bearer was the peer-global OAuth token (tier 2), which is the only
credential a peer-global refresh legitimately replaces. A 401 against a tier-1
or tier-3 bearer is surfaced to the caller; retrying it with the peer-global
grant would present a credential the operator never scoped to this workspace.
The resolver reports the tier it used (`peer_tokens::CredentialTier`), and every
outbound path asks the same question of it —
`peer_tokens::ResolvedBearer::allows_peer_global_refresh` — rather than
inferring the answer from `can_refresh(peer)`, which cannot see which tier was
used. Both outbound paths are bound by this:

| path | retry gate |
| ---- | ---------- |
| `peer_tokens::send_mcp_request_with_session` / `_with_state` (federation, panels, slices) | `peer_global_refresh_applies` |
| `connectors::mcp_client::jsonrpc_call_once` (connector poll and invoke) | `try_refresh_bearer_token` |

The connector path is not an edge case: `services::peer::auto_create_connector_for_peer`
writes connector config with `mcp_url`/`peer_id`/`workspace_id` and **no**
`bearer_token`, so every subscribed peer resolves through the full precedence
chain and a tier-1 delegated token is the ordinary bearer there.

## Cache scope

A peer may answer `tools/list`, `resources/list` and `slice://` differently per
credential — that is what per-(workspace, peer) delegation is for. So
`AppState::peer_manifest_cache` and `AppState::peer_slice_cache` are keyed by
`state::PeerCacheKey` = (`peer_id`, `Option<workspace_id>`), where the workspace
is the handle's `workspace_scope`, i.e. exactly the input outbound auth resolves
from. A peer-global fetch and a workspace-scoped fetch never share an entry, and
two workspaces never share one.

The key uses the resolution *input* rather than the resolved bearer: keying on
the bearer would mean resolving (and possibly refreshing) a token before every
cache read and putting credential material in a map key, and it would only ever
merge entries the workspace key already keeps apart safely.

Consequences:

- `peers.last_manifest_jsonb` is a single peer-global column, so only a
  peer-global fetch writes it and only a peer-global read falls back to it. A
  workspace-scoped fetch that fails with a cold cache surfaces the error instead
  of being handed a listing fetched under a credential it was never granted.
  Boot rehydration (`federation::hydrate_manifest_cache`) therefore loads it
  under the peer-global key only.
- A peer-global manifest refresh — the scheduler tick, `tools/list_changed`,
  `resources/list_changed`, and the admin force-refresh — drops every
  workspace-scoped entry for that peer, since none of them can be re-fetched
  without its own credential. `resources/updated` drops every slice entry for
  the peer for the same reason.
- The key space is (workspaces × peers), so it is bounded the same way the
  slice cache already was: slice entries are dropped at `SLICE_TTL_SECONDS`, and
  manifest entries at `MANIFEST_RETENTION_SECONDS` (one hour), which is past the
  five-minute TTL because an over-TTL entry is still served, marked `stale`,
  when the peer is unreachable.
- `federation::reindex_peer_catalog` writes org-scoped `peer_catalog_entries`
  rows, so its `sample_queries` may only come from the **peer-global** slice. It
  fetches one when the peer-global manifest cache is fresh (i.e. the caller just
  completed a peer-global round trip) and otherwise keeps the values already
  indexed, rather than emptying them whenever the slice cache is cold — which
  would flip `content_hash` and rewrite every row on every tick.

## Workspace scope

The credential is per `(workspace, peer)`, so outbound auth must know which
workspace a request is being made for. `models::Peer` carries a
`workspace_scope: Option<Uuid>` field — not a `peers` column, just the scope the
handle was resolved under, set by `Peer::scoped_to`. A handle without it cannot
resolve tier 1 or tier 3 at all, so every path that has a workspace must tag its
handle or it silently presents a peer-global credential instead.

`WorkspacePeerBindingRepo::list_active_peers_for_workspace` tags every peer it
returns, which covers the workspace data paths (table data, chart data, map
layers, table/chart/document panels, and `federation::workspace_context_slices`).
These set it explicitly:

| path | scope source |
| ---- | ------------ |
| `federation::route_tool_call_with_session` (federated `tools/call`, and the manifest lookup it does first) | the calling workspace |
| `federation::execute_pending_tool_call` (approved peer-tool execution) | `pending.workspace_id` |
| `federation::workspace_peer_manifest` / `workspace_peer_resources` | the requested workspace |
| `federation::expand_tool_schema` | the calling workspace |
| `routes::peers::subscribe_peer` | the subscribing workspace |
| `services::workspace_peer_binding` refresh | `binding.workspace_id` |
| `connectors::mcp_client` | the connector's `workspace_id` |

Peer-global request paths have no workspace and therefore resolve only on tiers 2
and 4 — they skip the workspace-scoped tiers 1 and 3:

- the registration-time `tools/list` manifest fetch, which runs before any
  workspace is bound;
- boot-time `federation::hydrate_manifest_cache` and the notification-driven
  `refresh_manifest_if_changed` / admin `force_refresh_manifest`, which run over
  a peer regardless of which workspaces are bound to it;
- the long-lived SSE notification session (`connectors::peer_session`), one per
  peer, shared across every workspace bound to it.

Guessing a workspace on those paths would present a credential the operator
scoped elsewhere.

Fail-closed poll behavior is unchanged: `mcp_client` still refuses to poll without
an Active binding (TT-A06). A credential does not substitute for a binding.

## Tests

`tests/peer_credential_integration.rs` — 11 tests, DB-backed, `#[ignore]`, serial.
Covers header presentation, workspace isolation on a shared peer, encryption at
rest plus non-echo on every read surface, rotation without a migration, the
precedence rule, audit-without-secrets, `peers:manage` 403, and cross-org 404.
Every one of them drives outbound auth through the `/table-data` route.

`tests/credential_presentation_integration.rs` — DB-backed, `#[ignore]`, serial.
Asserts the same header on the **federation** paths specifically, because
`/table-data` alone cannot catch a federation path that forgot to scope its peer
handle: federated `tools/call` (static credential and delegated token), workspace
isolation on that path, approved peer-tool execution, the no-downgrade rule on a
401, the connector's delegated-over-literal precedence, and the slice-cache TTL.

The same file covers cache scope. Those tests deliberately do **not** pre-seed
the cache — a pre-seeded cache is what made the peer-id-only key invisible to
every other test in the file — but drive the real fetch against a stub peer that
answers `tools/list`, `resources/list` and `slice://` differently per bearer:

- `workspace_manifest_is_not_served_to_another_workspace`
- `workspace_context_slice_is_not_served_to_another_workspace`
- `workspace_scoped_manifest_is_not_persisted_or_rehydrated_for_other_workspaces`
  (asserts `last_manifest_jsonb` stays null after a workspace-scoped fetch, then
  that a rehydrated peer-global manifest is not served to a workspace)
- `connector_does_not_retry_a_401_delegated_token_with_the_peer_global_token`
  and its tier-3 twin, asserting the exact bearers the peer saw, in order
- `catalog_reindex_keeps_sample_queries_when_the_slice_cache_is_cold`
