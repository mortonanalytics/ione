# Design — Contract parity between production and the conformance kit

**Date:** 2026-08-29
**Status:** Implemented, partially. Covered rules are listed below; the rest are in issue #62.
**Source:** GitHub issue #24
**Layers:** test only — no production behaviour changes
**Related:** [peer-conformance-kit.md](peer-conformance-kit.md), [app-integration-contract-v1.md](app-integration-contract-v1.md)

---

## Problem

`src/bin/ione-conformance.rs` restates IONe's contract rules in its own code.
On PR #23 alone, six production fixes each left the kit asserting the opposite
of what production did, and every one was caught by an adversarial review pass
rather than a test:

| Production change | Kit then said |
|---|---|
| `approval_required` made optional | FAIL if absent |
| Panel paths follow `nextCursor` | FAIL if the peer paginates |
| Client speaks streamable-HTTP | Could not parse an SSE-framed reply |
| `hex::decode` accepts `A-F` | FAIL on uppercase digest |
| `foreign_roles: null` accepted | FAIL on `null` |
| `registration_endpoint` optional | FAIL if absent |

Every one tells a **conforming** peer it is broken, and the cost lands on the
peer author, who has no way to know the kit is wrong.

## Constraint

The kit has zero `ione::` imports on purpose: an app team must be able to lift
that one file into a standalone crate and validate against it without building
IONe. That constraint is what forces the duplication, and this design keeps it.

## Decision

Option 3 from the issue — **contract-parity tests**. For each rule, one test
runs the same input vector through the production predicate and the kit's, and
asserts they agree.

Not option 1 (extract an `ione-contract` crate): it is the better end state and
also a restructuring, which is not what a rule-drift bug is worth today. Not
option 2 (generate the kit from a machine-readable contract): the contract is
prose, and making it machine-readable is a larger project than the drift.

Option 3 is also, per the issue, the smallest thing that would have caught all
six.

## Mechanism

`src/contract_parity.rs`, compiled only under `cfg(test)`, alongside:

```rust
#[cfg(test)]
#[path = "bin/ione-conformance.rs"]
mod conformance_kit;
```

Including the kit's source as a module reaches its private predicates without
the kit importing anything — the liftability property is untouched, because
nothing is added to that file. Living in the lib rather than `tests/` also
reaches production's `pub(crate)` predicates without widening any API for the
benefit of a test.

The cost: the kit's own `#[cfg(test)] mod tests` compiles into the lib test
binary too, so its unit tests run in both places. Milliseconds, and the
alternative is either widening production visibility or giving the kit an
`ione::` import.

## Covered rules

| Rule | Production | Kit |
|---|---|---|
| `approval_required` optional, defaults false | `routes::webhooks::WebhookEnvelope` | `verify_envelope` |
| Cursor termination — `null` and `""` are terminal, `cursor` is the legacy spelling | `services::federation::next_cursor` | `next_cursor` |
| SSE events joining multi-line `data:` | `services::peer_tokens::sse_event_payloads` | `sse_event_payloads` |
| Panel URL validation — https-only, SSRF guard, loopback and private allowed | `util::url_guard` | `validate_panel_url` |

Three of the six historical failures are directly covered (`approval_required`,
pagination, SSE). Their vectors are the exact shapes that broke.

## Not covered yet

`foreign_roles: null`, `registration_endpoint` optional, and the signature
grammar's hex case have no production predicate reachable as a pure function —
they live inside route handlers and async discovery paths. Pairing them means
either an HTTP-level test or extracting the predicate, both of which are more
than a parity harness should decide on its own. Issue #62 carries them.

## Acceptance

- Changing a covered production rule without changing the kit fails a test.
- The kit still has zero `ione::` imports.
- The uncovered rules are named, not silently absent.
