# Design — Finishing the RLS org-context sweep

**Date:** 2026-08-29
**Status:** Phase 1 implemented. Phase 2 is issue #64, phase 3 is #65 and is blocked on it.
**Source:** GitHub issue #25
**Layers:** `db`, `api`
**Related:** [identity-broker.md](identity-broker.md) "RLS activation" (AC-15), migration `0050`

---

## Where this started

Migration `0050` made row-level security *provably* enforceable: `FORCE ROW
LEVEL SECURITY` on all eleven org-scoped tables, a restricted `ione_app` role
that is neither `SUPERUSER` nor `BYPASSRLS`, and `rls::org_scoped_tx` to set
`app.current_org_id` per transaction. `tests/rls_enforcement_integration.rs`
proves isolation by reading `broker_credentials` with **no `org_id` predicate**,
so RLS is demonstrably the only filter.

It was adopted in part. Three things remained: the deployment still connects as
a bypassing role, `ione_app` was not runnable, and most queries never set the
context.

## The shape of the remaining work

Every org-scoped query falls into one of three cases, and they need different
answers:

1. **An org id is in hand.** Thread it and open an `org_scoped_tx`. Mechanical.
2. **An org id is one read away.** The caller holds something that knows its own
   org — usually a `Peer`, which carries `org_id`. Read that first, then scope.
   The OAuth callback is the clearest instance: it arrives with a nonce, no
   session and no org, but the pending row names a peer and the peer names the
   org.
3. **Neither.** A lookup keyed by a bearer token hash or a one-time nonce, where
   resolving the row *is* how the org becomes known. These cannot be scoped
   without a resolver that runs outside RLS.

Case 3 is the one that needs a decision rather than labour, and this design
takes it: **a `SECURITY DEFINER` resolver per lookup, returning only an org id**
— never a data row. The caller then opens an ordinary org-scoped transaction for
the real read. A resolver is a small, auditable function whose entire output is
one uuid, which is a far smaller hole than leaving the table's queries
unscoped. No resolver exists yet; #64 introduces the first one.

Nothing here is exempt-with-no-reason. A table either scopes, resolves, or has a
written reason in the table below.

## Inventory

All eleven `FORCE`d tables, with what each needs.

| Table | State | Case | Where |
|---|---|---|---|
| `broker_credentials` | partly scoped | 1, and 3 for the state lookups | `create_pending`, `list_for_user`, `find_for_user`, `store_tokens`, `delete` scoped in #23. `find_by_state`, `consume_by_state`, `find_user_provider` are case 3 |
| `workspace_peer_credentials` | **scoped** | 1, 2 | `get`, `list_for_workspace`, `delete` in #23; `upsert` and `secret_for` in this phase |
| `workspace_peer_delegations` | **scoped** | 1, 2 | `get`, `delete` in #23; `upsert_tokens`, `update_refreshed`, `material_for` in this phase |
| `peers` | unscoped | 1 | `PeerRepo` — most call sites already hold an org |
| `workspace_peer_bindings` | unscoped | 1, 2 | `WorkspacePeerBindingRepo`, `webhook_ingress` (case 2 — the peer names the org) |
| `service_account_tokens` | unscoped | 3 | authentication by token hash. Called out separately as HP-M4 in [headless-provisioning.md](headless-provisioning.md) |
| `auto_exec_policies` | unscoped | 1 | workspace-scoped call sites |
| `interaction_events` | unscoped | 1 | writers carry a workspace |
| `peer_catalog_entries` | unscoped | 1, 2 | `catalog_repo`, `federation` |
| `mfa_enrollments`, `mfa_recovery_codes` | unscoped | 1 | `routes::mfa`, which has a session |
| `identity_audit_events` | unscoped | 1 | `identity_audit_writer` |

## Phase 1 — this change

**Policies fail closed quietly.** Every policy read
`current_setting('app.current_org_id', true)::uuid`, which has two runtime
shapes: `NULL` on a fresh connection, and the empty string on one recycled from
an org-scoped transaction. Both fail closed, but the second raises `22P02` from
*inside the policy*, surfacing as an opaque database error rather than an empty
result. Migration `0051` wraps the read in `NULLIF(..., '')`, collapsing the
second shape onto the first.

This is the precondition for everything else. Under `ione_app` every not-yet
scoped query hits that error, so the noisy shape is what stands between IONe and
a runnable restricted role — not a leak, but an unreadable failure mode on every
unmigrated path.

**Two tables finished.** `workspace_peer_credentials` and
`workspace_peer_delegations` are now fully scoped, using case 1 where the caller
had an org and case 2 where it held a `Peer`. The OAuth callback reads the peer
to learn the org before writing the delegation.

## Phase 2 — #64

The remaining nine tables, table at a time, case 1 first because it is
mechanical. The first `SECURITY DEFINER` resolver lands with
`service_account_tokens`, which is the cleanest case-3 instance: authentication
by token hash, where the org is the *result* of the lookup.

## Phase 3 — #65

Cut `DATABASE_URL`, `docker-compose.yml`, CI and the dev loop over to `ione_app`
and run the full suite under it. This can only happen after phase 2: under a
non-bypassing role an unscoped query does not leak, it returns nothing, so an
unmigrated write would silently affect zero rows. Fail-closed is the right
direction and still the wrong behaviour.

Only after phase 3 can AC-15 in `identity-broker.md` be marked satisfied without
qualification.

## Acceptance for phase 1

- A policy read with an empty org context returns no rows instead of raising.
  This is the test that fails without `0051`.
- The credential and delegation reads that gained an org id work under
  `ione_app`, where RLS is the only filter, and org B's context cannot read org
  A's row.
- Every remaining table is named above with its case, so nothing is silently
  outstanding.
