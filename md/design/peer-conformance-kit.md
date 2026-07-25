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
| 1 | MCP server endpoint (§1) | `initialize` returns 2xx + a JSON-RPC 2.0 `result`; the `MCP-Session-Id` header is adopted if present; `tools/list` and `resources/list` terminate their `nextCursor` chain within IONe's 50-page cap; every resource has a non-empty `uri`; an unknown URI answers JSON-RPC **-32002 inside an HTTP 200**; `tools/call` exists. |
| 2 | OAuth 2.1 (§2) | `/.well-known/oauth-authorization-server` is served and declares `issuer`, `authorization_endpoint`, `token_endpoint`, `revocation_endpoint`, PKCE `S256`, and the `authorization_code` + `refresh_token` grants; the token endpoint rejects an unsupported grant with a 4xx OAuth error body. The authorization endpoint is interactive and cannot be driven headlessly, so it is not exercised. |
| 3 | Signed webhook sender (§3) | The kit runs a receiver implementing IONe's verification and checks: `X-IONe-Signature` grammar (only `t`/`v1`, no repeats), `v1` is exactly 64 lowercase hex, the HMAC over `t_ascii ++ "." ++ raw_body`, `abs(now − t) ≤ 300`, `abs(t − occurred_at) ≤ 30`, the path `peer_id`, the 256 KiB body cap, and every §3.3 envelope constraint including `approval_required` being present and boolean. |
| 4 | Resource view metadata (§4) | Re-implements all four extractors. Each `ione_view` resource is checked against the exact rule that decides whether IONe renders or **silently drops** it: `tile_url` non-empty for `map`; a complete nested spec for `chart` plus a `resources/read` of the body (`spec` object, `rows` array of objects, ≤ 2 MiB); a `resources/read` of the `table` body (ordered `schema`, permitted column types, ≤ 64 columns, ≤ 5 000 rows, ≤ 2 MiB); https-only SSRF-safe `download_url` and a resolvable mime type for `document`. An unknown `ione_view` value fails, because IONe would drop it with no diagnostic. |
| 5 | Context slice (§5) | `slice://` is readable, `mimeType` is `application/vnd.ione.slice+json`, `schema_version` is `"1"` (not `"0"`, which means IONe synthesized it), `summary` is non-empty, the serialized slice is ≤ 2 KiB (IONe truncates rather than rejects), `tool_index` entries carry a `name`, and no prompt-fence sentinel substrings are present. |
| 6 | `whoami://` (§6) | Readable, `mimeType` is `application/vnd.ione.whoami+json`, **all seven** keys present (a null value is fine; a missing key is not), `foreign_tenant_id` is a non-empty binding key, and `foreign_roles` is an array. |

## Reference stub peer

`tests/support/stub_peer.rs` is a faithful minimal implementation of the same six
surfaces, used as the fixture that proves both this kit and IONe's shell. It
serves one canned resource per `ione_view`, a peer-authored slice, a whoami
identity, a two-page `tools/list`, a functioning OAuth 2.1 authorization server
with real PKCE S256 verification, and a signed webhook sender.

Two things about it are deliberate and worth copying:

- **`resources/list` is a single page.** IONe's four panel paths read page 1 only
  (§8.2), so a peer that paginates its resource listing renders only its first
  page. `tools/list` paginates, because the manifest path does follow cursors.
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

## Findings — where IONe diverges from frozen v1

Recorded here rather than fixed, because the contract is frozen and these files
are owned elsewhere. Each is a **code vs. frozen-contract** gap, not a gap in the
kit.

### F1 — `nextCursor: null` on the final page loops IONe to its 50-page cap

Contract §8.1: *"Termination: the peer **omits** `nextCursor` (or sets it to
`null`) on the final page."*

`paginated_list` in `src/services/federation.rs` does:

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

Fix is one filter: `.get("nextCursor").filter(|value| !value.is_null())`.
The kit's own pagination follower already does this, which is why it reports the
stub as terminating cleanly.

### F2 — the webhook success body uses `signal_ids`, not `signalIds`

Contract §3.5 documents the 200 body as
`{"ok": true, "duplicate": false, "signalIds": ["<uuid>", ...]}`.

`WebhookAckResponse` in `src/routes/webhooks.rs` derives `Serialize` with no
`#[serde(rename_all = "camelCase")]`, so the field emits as `signal_ids`. `ok`
and `duplicate` are single words and are unaffected, which is why the divergence
is easy to miss.

Under the §"Compatibility rule" this is a v1 promise ("do not rename or remove
any required field"), so the frozen document should win and the struct should
gain the rename. `tests/stub_peer_conformance_integration.rs` accepts either key
so it does not pin the current behavior in place.

### F3 — not a divergence: `POST .../bindings/:id/refresh` now requires `peers:manage`

Added on this branch by `b4e2fef`. It is correct, but the seeded bootstrap
`member` role carries no grants, so a test or demo flow that calls the endpoint
as the default local user gets 403 until the role is granted `peers:manage`.
Noted because it is invisible from the contract, which says nothing about IONe's
own RBAC.
