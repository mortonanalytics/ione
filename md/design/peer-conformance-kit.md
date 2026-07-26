# Peer Conformance Kit

**Status:** Active
**Target contract:** [app-integration-contract-v1.md](app-integration-contract-v1.md) (v1, frozen 2026-07-25)
**Binary:** `src/bin/ione-conformance.rs` → `ione-conformance`
**Reference peer:** `tests/support/stub_peer.rs`
**Executable proof:** `tests/stub_peer_conformance_integration.rs`

## What this is

A candidate peer — TerraYield, GroundPulse, anyone — needs to know whether it
satisfies contract v1 **before** an IONe deployment exists in its loop. The
conformance kit is a self-contained checker it points at its own endpoint. It
reports PASS/FAIL per surface, explains each failure in terms of what IONe would
do (drop the resource, report 502, truncate the slice), and exits non-zero if any
surface failed.

It never contacts an IONe deployment. Every rule it asserts is re-implemented
from the frozen contract.

## Why a Rust `[[bin]]` and not a script

The kit ships as a bin target in this repo rather than a shell/Python script for
three reasons:

1. **No new dependency, in either direction.** It uses only crates IONe already
   depends on (`axum`, `reqwest`, `serde_json`, `chrono`, `hmac`, `sha2`, `hex`,
   `url`, `tokio`). It adds nothing to `Cargo.toml` — the file lives at
   `src/bin/ione-conformance.rs`, so Cargo's target auto-discovery names the
   binary for it. A script would have made `python3` or `jq` a build-time
   prerequisite of the automated test that proves the kit works.
2. **The proof is mechanical.** `tests/stub_peer_conformance_integration.rs`
   invokes the compiled binary through `env!("CARGO_BIN_EXE_ione-conformance")`
   and asserts all six surfaces PASS against the reference stub. A kit that is
   not itself tested is a liability.
3. **It is genuinely liftable.** The file imports nothing from the `ione` crate.
   Anyone who does not want to build IONe to run it can copy the single file into
   a new crate whose `Cargo.toml` lists the nine crates above, and it compiles
   unchanged.

The webhook surface (§3) needs a real HTTP receiver that verifies HMAC over raw
bytes with a constant signing input. Doing that correctly in `bash` + `openssl`
is exactly the kind of thing that produces a checker with the same bug as the
code under test.

## Usage

```bash
# Simplest: an MCP endpoint with pre-brokered static credentials.
cargo run --bin ione-conformance -- \
  --url https://app.example.com/mcp \
  --token "$IONE_PEER_TOKEN" \
  --pre-brokered

# Full run, including the OAuth 2.1 discovery surface and a webhook round-trip.
cargo run --bin ione-conformance -- \
  --url https://app.example.com/mcp \
  --token "$IONE_PEER_TOKEN" \
  --webhook-peer-id 3f0c1f2e-8a1b-4c2d-9e3f-0a1b2c3d4e5f \
  --webhook-secret "$IONE_WEBHOOK_SIGNING_SECRET" \
  --webhook-trigger https://app.example.com/internal/emit-test-event \
  --webhook-timeout 30
```

| Flag | Meaning |
|---|---|
| `--url <URL>` | **Required.** The peer's MCP endpoint (§1). |
| `--token <BEARER>` | Bearer presented on every MCP request (§1). Omit for an unauthenticated peer. |
| `--pre-brokered` | Declare that this deployment uses static per-`(workspace, peer)` credentials, so §2 does not apply. Surface 2 then reports SKIP. |
| `--oauth-issuer <URL>` | Origin hosting the OAuth metadata, when it is not the origin of `--url`. |
| `--webhook-peer-id <UUID>` | The `peer_id` IONe issued you (§3). |
| `--webhook-secret <SECRET>` | The `signingSecret` IONe provisioned for you (§3). |
| `--webhook-trigger <URL>` | Optional. An endpoint on **your** app the kit POSTs `{"ione_base_url": "<kit receiver>"}` to, so the run is non-interactive. Not part of contract v1 — a test affordance. |
| `--webhook-timeout <SECS>` | How long to wait for the event. Default 30. |

Exit codes: `0` nothing failed, `1` at least one surface failed, `2` bad usage.

Surface 3 is optional in v1. Omitting both webhook flags makes it SKIP, and a
SKIP never fails the run.

## What each surface checks

| # | Surface | Checks |
|---|---|---|
| 1 | MCP server endpoint (§1) | `initialize` returns 2xx + a JSON-RPC 2.0 `result`; the `MCP-Session-Id` header is adopted if present; `tools/list` and `resources/list` terminate their `nextCursor` chain within IONe's 50-page cap (absent, `null` **and the empty string** all terminate, §8.1); every resource has a non-empty `uri`; an unknown URI answers JSON-RPC **-32002 inside an HTTP 200**; `tools/call` exists. Every reply is correlated by JSON-RPC id. |
| 2 | OAuth 2.1 (§2) | `/.well-known/oauth-authorization-server` is served and declares `issuer`, `authorization_endpoint`, `token_endpoint`, `revocation_endpoint`, PKCE `S256`, and the `authorization_code` + `refresh_token` grants; the token endpoint rejects an unsupported grant with a 4xx OAuth error body. The authorization endpoint is interactive and cannot be driven headlessly, so it is not exercised. |
| 3 | Signed webhook sender (§3) | The kit runs a receiver implementing IONe's verification and checks: `X-IONe-Signature` grammar (only `t`/`v1`, no repeats, each key and value trimmed), `v1` is exactly 64 hex characters **case-insensitively** (`hex::decode` accepts `A-F`, so IONe does), the HMAC over the **raw `t=` text** ++ `"."` ++ raw_body, `abs(now − t) ≤ 300`, `abs(t − occurred_at) ≤ 30`, the path `peer_id`, the 256 KiB body cap, and every §3.3 envelope constraint. `approval_required` is checked as **optional** per §3.3 — absent passes and is reported as defaulting to `false`; present-but-not-boolean fails. |
| 4 | Resource view metadata (§4) | Re-implements all four extractors. Each `ione_view` resource is checked against the exact rule that decides whether IONe renders or **silently drops** it: for `map`, a non-empty `tile_url` that is https and passes the SSRF guard (an unsafe one drops the whole layer, so it FAILs) plus an informational note when an unsafe `vector_url` would be **stripped** (the layer survives, so it does not fail); a complete nested spec for `chart` plus a `resources/read` of the body (`spec` object, `rows` array of objects); a `resources/read` of the `table` body (ordered `schema`, permitted column types, ≤ 64 columns, ≤ 5 000 rows); https-only SSRF-safe `download_url` and a resolvable mime type for `document`. Both body reads are held to IONe's 2 MiB cap **and** its 5-second timeout, measured the way `chart_data.rs`/`table_data.rs` measure them (declared `Content-Length` before buffering, `contents[0].text` after — never a re-serialization of the parsed value). An unknown `ione_view` value fails, because IONe would drop it with no diagnostic. |
| 5 | Context slice (§5) | `slice://` is readable, `mimeType` is `application/vnd.ione.slice+json`, `schema_version` is `"1"` (not `"0"`, which means IONe synthesized it), `summary` is non-empty, the serialized slice is ≤ 2 KiB (IONe truncates rather than rejects), `tool_index` entries carry a `name`, and no prompt-fence sentinel substrings are present. |
| 6 | `whoami://` (§6) | Readable, `mimeType` is `application/vnd.ione.whoami+json`, **all seven** keys present (a null value is fine; a missing key is not), `foreign_tenant_id` is a non-empty binding key, every optional scope is a string or null, and `foreign_roles` is an array of strings — see [F5](#f5--open-whoamis-foreign_roles-does-not-accept-the-null-6-permits) for why null is the one exception. |

## Transport parity

The kit is only trustworthy if its wire behavior is IONe's wire behavior. A kit
that is *easier* to satisfy than production passes a peer that then fails in the
field; a kit that is *harder* tells a correct peer it is broken. Four things are
therefore copied from `src/services/peer_tokens.rs` and `src/util/url_guard.rs`
rather than reimplemented loosely:

- **Both framings.** Every POST advertises
  `Accept: application/json, text/event-stream` and `MCP-Protocol-Version`, and
  the reply is read by sniffing `Content-Type`: a plain JSON object, or an SSE
  stream whose `data:` frames may be preceded by unrelated server-initiated
  requests and notifications. Streamable HTTP is the MCP spec's default
  transport and IONe supports it, so a kit that only parsed plain JSON reported
  `"initialize response is not JSON"` against a fully conforming peer — and a
  strict server is entitled to answer **406** to the missing `Accept` outright.
- **Id correlation.** Each request gets its own id
  (`peer_tokens::next_request_id`) and the reply is matched against it, with the
  JSON-RPC null-id error reply accepted as unambiguous. A peer that hardcodes a
  reply id fails IONe's manifest refresh, so it must fail here.
- **No redirects.** `url_guard::guarded_client` sets `redirect::Policy::none()`,
  so a peer whose MCP endpoint answers a 3xx cannot federate. The kit disables
  redirect following too and names the 3xx rather than reporting the redirect
  body as malformed JSON.
- **Cursor termination.** `next_cursor` is term-for-term
  `federation::next_cursor` / `peer_panels::next_cursor`: `cursor` is an alias
  for `nextCursor`, resolved *before* the filters, and absent / `null` / `""` all
  terminate.

## Reference stub peer

`tests/support/stub_peer.rs` is a faithful minimal implementation of the same six
surfaces, used as the fixture that proves both this kit and IONe's shell. It
serves one canned resource per `ione_view`, a peer-authored slice, a whoami
identity, a two-page `tools/list`, a functioning OAuth 2.1 authorization server
with real PKCE S256 verification, and a signed webhook sender.

Two things about it are deliberate and worth copying:

- **`resources/list` is a single page, `tools/list` is two.** Both are conforming
  shapes and the stub exercises one of each. Since issue #18 all four panel paths
  follow `nextCursor` via `peer_panels.rs` (§8.2), so a paginating
  `resources/list` now renders every page up to the 50-page cap — the kit's
  own pagination cases live in `src/bin/ione-conformance.rs`'s test module.
- **`POST /mcp` does not gate on the bearer token.** §1 permits an
  unauthenticated peer, and leaving it open lets one fixture be driven both
  pre-brokered and unauthenticated. A production peer must validate the token it
  issued via §2.

`tests/stub_peer_conformance_integration.rs` asserts that IONe renders all four
of the stub's views end to end, reads its chart and table bodies, binds its
`whoami://` identity, surfaces its peer-authored slice, follows its `tools/list`
pagination, and accepts its signed webhook — with no stub-specific code anywhere
in IONe.

```bash
SQLX_OFFLINE=true IONE_SKIP_LIVE=1 \
  DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  cargo test --test stub_peer_conformance_integration -- --ignored --test-threads=1
```

## Findings — where IONe diverged from frozen v1

Each was a **code vs. frozen-contract** gap, not a gap in the kit. F1 and F2 were
recorded here as live divergences and have since been **resolved in code**; the
history is kept because it explains why the regressions that pin them exist. F4
and F5 are open. Verify against the cited source before citing a finding as
current.

Gaps that were in the **kit** rather than in IONe are not listed here — they are
regressions in `src/bin/ione-conformance.rs`'s test module, each named after the
conforming peer shape it would otherwise have failed.

### F1 — RESOLVED: `nextCursor: null` on the final page looped IONe to its 50-page cap

Contract §8.1: *"Termination: the peer **omits** `nextCursor` (or sets it to
`null`) on the final page."*

`paginated_list` in `src/services/federation.rs` used to do:

```rust
cursor = result.get("nextCursor").cloned().or_else(|| {
    result.get("cursor").filter(|value| !value.is_null()).cloned()
});
if cursor.is_none() { break; }
```

`result.get("nextCursor")` returns `Some(&Value::Null)` when the key is present
with a null value, so `.cloned()` yields `Some(Value::Null)` and the loop does
not terminate. The `!value.is_null()` filter that would have caught it is applied
only to the `cursor` alias.

Reproduced against the stub peer: with page 2 answering `"nextCursor": null`,
IONe issued the full 50 pages and returned each tool 25 times
(`["query_displacement", "acknowledge_alert", "query_displacement", …]`, 50
entries) instead of 2. A peer that follows the contract literally gets 50×
request amplification and a duplicated manifest.

Fixed by one filter. `next_cursor` in `src/services/federation.rs` now treats an
absent, `null` or empty-string cursor as terminal for both the `nextCursor`
spelling and the `cursor` alias, and its unit test
`next_cursor_treats_null_and_empty_as_terminal` pins all four cases. The kit's
own pagination follower always did this, which is why it reported the stub as
terminating cleanly.

### F2 — RESOLVED: the webhook success body now uses `signalIds`

Contract §3.5 documents the 200 body as
`{"ok": true, "duplicate": false, "signalIds": ["<uuid>", ...]}`.

When this was first written, `WebhookAckResponse` in `src/routes/webhooks.rs`
derived `Serialize` with no `#[serde(rename_all = "camelCase")]`, so the field
emitted as `signal_ids`. `ok` and `duplicate` are single words and were
unaffected, which is why the divergence was easy to miss.

Under the §"Compatibility rule" this is a v1 promise ("do not rename or remove
any required field"), so the frozen document won and the struct gained the
rename. `WebhookAckResponse` now carries `#[serde(rename_all = "camelCase")]`
and emits `signalIds`. Both
`tests/phase_push_ingress.rs::webhook_ack_uses_the_camel_case_key_from_the_contract`
and `tests/stub_peer_conformance_integration.rs::ione_accepts_stub_peer_signed_webhook`
assert `signalIds` and assert `signal_ids` is absent, so deleting the rename
fails them.

### F4 — OPEN: IONe's OAuth discovery parser requires `registration_endpoint`, §2 does not

Contract §2 lists exactly four peer endpoints — discovery, authorize, token,
revoke — and grades the endpoint set **Specified**. `PeerDiscovery` in
`src/services/peer_oauth.rs` deserializes `registration_endpoint` as a plain
`String` with **no** `#[serde(default)]`, so a discovery document that omits it
fails to deserialize and the federation flow never begins. It does not read
`issuer` or `revocation_endpoint` at all.

A peer can therefore satisfy §2 exactly, pass this kit, and still be unable to
federate. Because the contract is frozen and does not name a registration
endpoint, the kit **reports** this on surface 2 rather than failing it: turning
an unfrozen requirement into a FAIL would itself be the false-FAIL class this kit
exists to prevent. Resolving it properly is a code-or-contract decision — either
`registration_endpoint` becomes `Option<String>`, or v1.x documents it as
required (additive under §9, since it is already enforced).

### F5 — OPEN: `whoami://`'s `foreign_roles` does not accept the null §6 permits

Contract §6: *"A value may be `null` where the peer has no such scope; a v1
consumer must tolerate `null` and must not treat a missing key and a null value
as equivalent."*

`WhoamiResponse` in `src/services/workspace_peer_binding.rs` types six of the
seven keys so that holds — `peer_id` and the four `foreign_*` scopes are
`Option<String>`, `foreign_tenant_id` is a required non-empty `String`. The
seventh, `foreign_roles`, is `#[serde(default)] Vec<String>`, which accepts an
**absent** key and rejects an explicit `null` with
`invalid type: null, expected a sequence`. That failure is not scoped to the
field: `serde_json::from_str::<WhoamiResponse>` fails outright, `fetch_whoami`
returns an error, and the operator is left a `pending` binding to complete by
hand — the exact outcome §6 describes for a *failed* whoami.

The kit fails a null `foreign_roles` and says why, because passing it would pass
a peer that cannot bind. Resolving it properly is again a code-or-contract
decision: `Option<Vec<String>>` with `.unwrap_or_default()`, or §6 narrowing its
null allowance to exclude this field.

### F3 — not a divergence: `POST .../bindings/:id/refresh` now requires `peers:manage`

Added on this branch by `b4e2fef`. It is correct, but the seeded bootstrap
`member` role carries no grants, so a test or demo flow that calls the endpoint
as the default local user gets 403 until the role is granted `peers:manage`.
Noted because it is invisible from the contract, which says nothing about IONe's
own RBAC.
