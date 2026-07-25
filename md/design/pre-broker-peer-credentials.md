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

Outbound bearer resolution, highest first
(`services::peer_tokens::resolve_access_token`):

1. the peer's brokered OAuth access token (`peers.access_token_ciphertext`),
2. the pre-broker credential for the request's workspace scope,
3. the process-global `IONE_OAUTH_STATIC_BEARER` env fallback.

**OAuth outranks the static credential deliberately.** When #12 lands and a peer
gains a brokered token, outbound auth switches on the very next request with no
flag day and no operator action; the now-dormant static credential can be deleted
whenever convenient. The reverse ordering would require every operator to delete
credentials in lockstep with the broker rollout.

In the `mcp_client` connector the credential also outranks the legacy
`bearer_token` literal in connector config, so rotating through the API takes
effect without rewriting connector rows.

## Workspace scope

The credential is per `(workspace, peer)`, so outbound auth must know which
workspace a request is being made for. `models::Peer` carries a
`workspace_scope: Option<Uuid>` field — not a `peers` column, just the scope the
handle was resolved under. `WorkspacePeerBindingRepo::list_active_peers_for_workspace`
tags every peer it returns, which covers the workspace data paths (table data,
chart data, map layers, table/chart/document panels, federation tool and resource
calls). `routes::peers::subscribe_peer`, the binding refresh path, and the
`mcp_client` connector set it explicitly.

Peer-global request paths have no workspace and therefore stay on tiers 1 and 3:
the registration-time `tools/list` manifest fetch (which runs before any workspace
is bound) and the long-lived SSE notification session (one per peer, shared across
every workspace bound to it). Guessing a workspace on those paths would present a
credential the operator scoped elsewhere.

Fail-closed poll behavior is unchanged: `mcp_client` still refuses to poll without
an Active binding (TT-A06). A credential does not substitute for a binding.

## Tests

`tests/peer_credential_integration.rs` — 11 tests, DB-backed, `#[ignore]`, serial.
Covers header presentation, workspace isolation on a shared peer, encryption at
rest plus non-echo on every read surface, rotation without a migration, the
precedence rule, audit-without-secrets, `peers:manage` 403, and cross-org 404.
