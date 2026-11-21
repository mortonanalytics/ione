-- Second case-3 resolver of the RLS org-context sweep (#64,
-- md/design/rls-org-context-sweep.md), following 0060.
--
-- The broker OAuth callback arrives with only the `state` token and no session:
-- resolving the state is how the org becomes known, so the lookup cannot run
-- inside an org-scoped transaction. This function runs as its owner (the
-- migrating role, which bypasses RLS) for exactly one reason: to turn a state
-- token into an org id. It returns that uuid and nothing else — never a row,
-- never the code verifier, the expiry or any token ciphertext. The caller then
-- opens an ordinary org-scoped transaction and reads or consumes the row under
-- that org's policy, where the expiry check still applies.
--
-- `broker_state_token_unique` makes state_token unique among non-NULL values,
-- so at most one row matches.
--
-- search_path is pinned so a caller cannot shadow `broker_credentials` with an
-- object in a schema it controls.
CREATE FUNCTION resolve_broker_credential_state_org(p_state_token TEXT)
RETURNS UUID
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
    SELECT org_id FROM public.broker_credentials WHERE state_token = p_state_token
$$;

REVOKE ALL ON FUNCTION resolve_broker_credential_state_org(TEXT) FROM PUBLIC;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        GRANT EXECUTE ON FUNCTION resolve_broker_credential_state_org(TEXT) TO ione_app;
    END IF;
END $$;
