CREATE TYPE binding_status AS ENUM ('active', 'pending', 'conflict', 'inactive');

CREATE TABLE workspace_peer_bindings (
    id                    UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID        NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    workspace_id          UUID        NOT NULL REFERENCES workspaces(id)   ON DELETE CASCADE,
    peer_id               UUID        NOT NULL REFERENCES peers(id)        ON DELETE RESTRICT,
    foreign_tenant_id     TEXT        NOT NULL DEFAULT '',
    foreign_tenant_name   TEXT        NULL,
    foreign_workspace_id  TEXT        NULL,
    foreign_user_id       TEXT        NULL,
    foreign_user_email    TEXT        NULL,
    foreign_roles         TEXT[]      NOT NULL DEFAULT '{}',
    scope                 JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status                binding_status NOT NULL DEFAULT 'pending',
    whoami_refreshed_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT wpb_unique_workspace_peer UNIQUE (workspace_id, peer_id),
    CONSTRAINT scope_is_object CHECK (jsonb_typeof(scope) = 'object')
);

CREATE OR REPLACE FUNCTION wpb_check_same_org() RETURNS trigger AS $$
DECLARE ws_org UUID; peer_org UUID;
BEGIN
    SELECT org_id INTO ws_org   FROM workspaces WHERE id = NEW.workspace_id;
    SELECT org_id INTO peer_org FROM peers      WHERE id = NEW.peer_id;
    IF ws_org IS DISTINCT FROM peer_org THEN
        RAISE EXCEPTION 'cross-org bindings are not allowed: workspace org % vs peer org %', ws_org, peer_org;
    END IF;
    NEW.org_id := ws_org;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpb_check_same_org_trg
BEFORE INSERT OR UPDATE OF workspace_id, peer_id ON workspace_peer_bindings
FOR EACH ROW EXECUTE FUNCTION wpb_check_same_org();

CREATE OR REPLACE FUNCTION wpb_touch_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at := now(); RETURN NEW; END $$ LANGUAGE plpgsql;

CREATE TRIGGER wpb_touch_updated_at_trg
BEFORE UPDATE ON workspace_peer_bindings
FOR EACH ROW EXECUTE FUNCTION wpb_touch_updated_at();

CREATE INDEX wpb_workspace_peer ON workspace_peer_bindings (workspace_id, peer_id);
CREATE INDEX wpb_peer_status    ON workspace_peer_bindings (peer_id, status);
CREATE INDEX wpb_org            ON workspace_peer_bindings (org_id);

ALTER TABLE workspace_peer_bindings ENABLE ROW LEVEL SECURITY;
CREATE POLICY wpb_org_isolation ON workspace_peer_bindings
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
