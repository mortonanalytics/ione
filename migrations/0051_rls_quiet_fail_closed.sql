-- RLS policies: fail closed *quietly* on an unset org context.
--
-- Every policy read `current_setting('app.current_org_id', true)::uuid`. That
-- has two shapes at runtime and only one of them is quiet:
--
--   * a fresh connection returns NULL     -> `org_id = NULL` -> no rows. Correct.
--   * a recycled pooled connection where a
--     previous transaction's SET LOCAL was
--     rolled back returns the empty string -> `''::uuid` raises 22P02.
--
-- Both fail closed — neither leaks a row — but the second raises an invalid
-- input syntax error from inside a policy, which surfaces as an opaque database
-- error rather than an empty result. That noise is the thing standing between
-- IONe and running as the restricted `ione_app` role, because every not-yet
-- org-scoped query hits it.
--
-- `NULLIF(..., '')` collapses the second shape onto the first. No policy's
-- semantics change for a connection that set the context: NULLIF only rewrites
-- the empty string, which was never a valid org id.
--
-- Issue #25. Migration 0050 declared FORCE and the restricted role; this makes
-- the policies survivable under it.

ALTER POLICY identity_audit_org_isolation ON identity_audit_events
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY mfa_enrollments_org_isolation ON mfa_enrollments
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY mfa_recovery_org_isolation ON mfa_recovery_codes
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY wpb_org_isolation ON workspace_peer_bindings
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY broker_credentials_org_isolation ON broker_credentials
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY aep_org_isolation ON auto_exec_policies
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY sat_org_isolation ON service_account_tokens
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY pce_org_isolation ON peer_catalog_entries
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY interaction_events_org_isolation ON interaction_events
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY wpc_org_isolation ON workspace_peer_credentials
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);

ALTER POLICY wpd_org_isolation ON workspace_peer_delegations
    USING (org_id = NULLIF(current_setting('app.current_org_id', true), '')::uuid);
