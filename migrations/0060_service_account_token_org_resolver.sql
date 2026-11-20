-- First case-3 resolver of the RLS org-context sweep (#64,
-- md/design/rls-org-context-sweep.md).
--
-- Bearer authentication arrives with a token hash and no org: resolving the
-- token is how the org becomes known, so the lookup cannot run inside an
-- org-scoped transaction. This function runs as its owner (the migrating role,
-- which bypasses RLS) for exactly one reason: to turn a token hash into an org
-- id. It returns that uuid and nothing else — never a row, never the token's
-- permissions, revocation or expiry. The caller then opens an ordinary
-- org-scoped transaction and reads the token under that org's policy.
--
-- search_path is pinned so a caller cannot shadow `service_account_tokens`
-- with an object in a schema it controls.
CREATE FUNCTION resolve_service_account_token_org(p_token_hash TEXT)
RETURNS UUID
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
    SELECT org_id FROM public.service_account_tokens WHERE token_hash = p_token_hash
$$;

REVOKE ALL ON FUNCTION resolve_service_account_token_org(TEXT) FROM PUBLIC;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        GRANT EXECUTE ON FUNCTION resolve_service_account_token_org(TEXT) TO ione_app;
    END IF;
END $$;
