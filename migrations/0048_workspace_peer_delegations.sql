-- Brokered delegated tokens per (workspace, peer) — issue #12.
--
-- Before this migration the only brokered peer token lived on `peers`
-- (`access_token_ciphertext`, migrations 0017/0032), so every workspace bound to
-- a peer presented the same delegated credential. The operator's delegation was
-- therefore peer-wide, not workspace-scoped, which is what issue #12 asks for
-- ("delegated-token storage per (workspace, peer)").
--
-- This table is additive: a peer with no row here keeps resolving its outbound
-- bearer exactly as before (peer-global OAuth token -> per-(workspace, peer)
-- static credential from #19 -> IONE_OAUTH_STATIC_BEARER). See the precedence
-- doc comment on `services::peer_tokens::resolve_access_token`.
--
-- Ciphertext uses the versioned envelope (`util::token_crypto::encrypt_versioned`,
-- `[1-byte key-version][12-byte nonce][ciphertext+tag]`) keyed on IONE_TOKEN_KEY,
-- matching `broker_credentials` and `workspace_peer_credentials`. The legacy
-- un-versioned envelope on `peers` is not used for new columns.

CREATE TABLE workspace_peer_delegations (
    id                       UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                   UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id             UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,
    peer_id                  UUID        NOT NULL REFERENCES peers(id)        ON DELETE CASCADE,
    -- The OAuth client id IONe was registered under with this peer. Held on the
    -- delegation row so a refresh does not depend on `peers.oauth_client_id`
    -- still holding the value the grant was issued against.
    oauth_client_id          TEXT        NOT NULL,
    -- Captured at grant time, after the host/scheme check against the peer's
    -- mcp_url. Refresh reuses it instead of re-reading the discovery document,
    -- so a peer that later publishes a different token endpoint cannot redirect
    -- a refresh of an already-issued grant.
    token_endpoint           TEXT        NOT NULL,
    access_token_ciphertext  BYTEA       NOT NULL,
    refresh_token_ciphertext BYTEA,
    token_expires_at         TIMESTAMPTZ,
    granted_by               UUID        REFERENCES users(id) ON DELETE SET NULL,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    refreshed_at             TIMESTAMPTZ,
    CONSTRAINT wpd_unique_workspace_peer UNIQUE (workspace_id, peer_id)
);

-- org_id is derived, never supplied — a delegation can only exist when the
-- workspace and the peer live in the same org. Mirrors wpc_check_same_org.
CREATE OR REPLACE FUNCTION wpd_check_same_org() RETURNS trigger AS $$
DECLARE ws_org UUID; peer_org UUID;
BEGIN
    SELECT org_id INTO ws_org   FROM workspaces WHERE id = NEW.workspace_id;
    SELECT org_id INTO peer_org FROM peers      WHERE id = NEW.peer_id;
    IF ws_org IS DISTINCT FROM peer_org THEN
        RAISE EXCEPTION 'cross-org peer delegations are not allowed: workspace org % vs peer org %', ws_org, peer_org;
    END IF;
    NEW.org_id := ws_org;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpd_check_same_org_trg
BEFORE INSERT OR UPDATE OF workspace_id, peer_id ON workspace_peer_delegations
FOR EACH ROW EXECUTE FUNCTION wpd_check_same_org();

CREATE INDEX wpd_org ON workspace_peer_delegations (org_id);

-- Org-isolation RLS, consistent with the other org-scoped tables. Inert today
-- (`app.current_org_id` is never set by the application, and the application
-- role owns the table while no table declares FORCE ROW LEVEL SECURITY); the
-- application `WHERE org_id = $n` predicate is the real guard. See the
-- "RLS activation" limitation in md/design/identity-broker.md.
ALTER TABLE workspace_peer_delegations ENABLE ROW LEVEL SECURITY;
CREATE POLICY wpd_org_isolation ON workspace_peer_delegations
    USING (org_id = current_setting('app.current_org_id', true)::uuid);

-- In-flight delegation authorizations. Same shape and hardening as
-- `peer_oauth_pending` (migration 0018): a 32-byte CSPRNG nonce carried in the
-- OAuth `state` parameter, a 10-minute TTL, and single-use consumption via
-- `DELETE ... RETURNING`, so a replayed or guessed state finds nothing.
CREATE TABLE workspace_peer_delegation_pending (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    peer_id         UUID        NOT NULL REFERENCES peers(id)      ON DELETE CASCADE,
    nonce           TEXT        NOT NULL UNIQUE,
    code_verifier   TEXT        NOT NULL,
    oauth_client_id TEXT        NOT NULL,
    redirect_uri    TEXT        NOT NULL,
    -- Captured at begin time, after the host/scheme check against the peer's
    -- mcp_url, so the callback cannot be steered to a different token endpoint
    -- by a discovery document that changed mid-flight.
    token_endpoint  TEXT        NOT NULL,
    granted_by      UUID        REFERENCES users(id) ON DELETE SET NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX wpdp_expires_at ON workspace_peer_delegation_pending (expires_at);
