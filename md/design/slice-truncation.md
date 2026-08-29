# Design — Slice truncation that a reader can see

**Date:** 2026-08-29
**Status:** Implemented.
**Source:** GitHub issue #28
**Layers:** `api` (prompt assembly only — no DB, no wire change)
**Related:** [app-integration-contract-v1.md](app-integration-contract-v1.md) §5.1, [mcp-federation.md](mcp-federation.md) Slice B

---

## Problem

`MAX_SLICE_BYTES = 2048` was applied as a byte-offset cut at prompt assembly.
A peer whose serialized slice exceeds 2 KiB had it sliced mid-payload and the
remainder fed to the model. Two failures, both quiet:

1. **Nobody is told.** Not the peer author, not the operator, not the model.
2. **The cut lands mid-structure.** A serialized JSON document cut at a byte
   offset is no longer JSON. The model reasons over a half-finished tool
   description inside a fenced block that claims to be a peer's capability
   summary.

The second is the one that matters. A truncated *document* is a smaller true
statement; a truncated *serialization* is a malformed one, and the model has no
way to tell which it got.

## Decision

Truncate structurally, and say so in the payload.

The issue offered three options — reject at read time, truncate at a structural
boundary, or keep byte truncation with a `warn!`. This takes the second and
also does the third's logging.

**Not rejection.** IONe reads the slice; the peer is not waiting on a response
it could be told about. Rejecting means the model loses the peer's summary
entirely because its tool list ran long, which is worse than a marked partial.
The conformance kit already checks the limit peer-side, so a conforming peer
never lands here.

**Not silent byte truncation.** It produces malformed JSON, which is the actual
defect.

## Behaviour

Given a peer's slice body, at prompt assembly:

1. Serialize, strip fence sentinels, and measure. Under the limit → unchanged.
   This is every conforming peer, and the path is byte-identical to before.
2. Over the limit and the body has a `tool_index` array → drop entries from the
   end until it fits, and set `_ione_truncated` to a sentence naming how many
   entries went and why.
3. Still over, or no `tool_index` → fall back to a minimal object carrying
   `summary` (shortened on a character boundary until it fits) and
   `_ione_truncated`.

Every path emits valid JSON, and every truncating path leaves the marker. The
marker is a sentence rather than a struct because its reader is a language
model: *"IONe dropped 12 of 40 tool_index entries to fit the 2048-byte slice
limit."*

A `warn!` carries `peer_id`, the original size, and the final size, so the
operator has a signal the peer author can be shown.

## What this does not do

**No interaction event.** `build_slice_context` is a pure function over
`SliceEntry` values with no `AppState`, org, or workspace in hand. Threading
those through to emit an event is a larger change than the defect warrants, and
the `warn!` plus the in-payload marker already clears the issue's floor ("at
minimum it should not be silent"). Worth revisiting if oversized slices turn
out to be common.

**No contract change.** §5.1 documents the 2 KiB limit and its enforcement; the
limit is unchanged and the enforcement description is updated to match. Nothing
on the wire moves, so no paired `eo_ag` issue.

## Acceptance

- An oversized slice yields parseable JSON. This is the test that would have
  failed before.
- The truncated payload states that it was truncated.
- A slice under the limit is untouched, sentinel stripping included.
- The conformance kit's message describes the same behaviour production has.
