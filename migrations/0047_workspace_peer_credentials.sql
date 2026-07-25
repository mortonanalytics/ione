-- Pre-broker peer credentials (issue #19): one static bearer credential per
-- (workspace, peer), encrypted at rest with the versioned IONE_TOKEN_KEY
-- envelope (util::token_crypto::encrypt_versioned).
--
-- Rotation is an UPDATE of credential_ciphertext on this row — a config/API
-- operation, never a schema change. The brokered-identity work (#12) supersedes
-- this table without changing the peer-side header contract: both modes present
-- `Authorization: Bearer <credential>`.
--
-- Separate table rather than a column on workspace_peer_bindings because
-- binding rows are serialized wholesale into API responses (routes/bindings.rs
-- returns `serde_json::to_value(binding)`), so a ciphertext column there would
-- ride along on every binding read; and because credential lifetime is
-- independent of binding status (a binding may sit pending/conflict while the
-- credential stays valid).

CREATE TABLE workspace_peer_credentials (
    id                    UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id          UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,
    peer_id               UUID        NOT NULL REFERENCES peers(id)        ON DELETE CASCADE,
    credential_ciphertext BYTEA       NOT NULL,
    created_by            UUID        REFERENCES users(id) ON DELETE SET NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    rotated_at            TIMESTAMPTZ NULL,
    CONSTRAINT wpc_unique_workspace_peer UNIQUE (workspace_id, peer_id)
);

-- org_id is derived, never supplied: a credential can only exist when the
-- workspace and the peer live in the same org. Mirrors wpb_check_same_org.
CREATE OR REPLACE FUNCTION wpc_check_same_org() RETURNS trigger AS $$
DECLARE ws_org UUID; peer_org UUID;
BEGIN
    SELECT org_id INTO ws_org   FROM workspaces WHERE id = NEW.workspace_id;
    SELECT org_id INTO peer_org FROM peers      WHERE id = NEW.peer_id;
    IF ws_org IS DISTINCT FROM peer_org THEN
        RAISE EXCEPTION 'cross-org peer credentials are not allowed: workspace org % vs peer org %', ws_org, peer_org;
    END IF;
    NEW.org_id := ws_org;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpc_check_same_org_trg
BEFORE INSERT OR UPDATE OF workspace_id, peer_id ON workspace_peer_credentials
FOR EACH ROW EXECUTE FUNCTION wpc_check_same_org();

CREATE INDEX wpc_org ON workspace_peer_credentials (org_id);

ALTER TABLE workspace_peer_credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY wpc_org_isolation ON workspace_peer_credentials
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
