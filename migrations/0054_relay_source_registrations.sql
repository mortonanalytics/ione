CREATE TABLE relay_source_registrations (
    org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    alias TEXT NOT NULL CHECK (alias ~ '^[a-z_][a-z0-9_]{0,62}$'),
    request_id UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, workspace_id, actor_id, alias),
    UNIQUE (request_id)
);
ALTER TABLE relay_source_registrations ENABLE ROW LEVEL SECURITY;
ALTER TABLE relay_source_registrations FORCE ROW LEVEL SECURITY;
CREATE POLICY relay_source_registrations_org ON relay_source_registrations
    USING (org_id = current_setting('app.current_org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('app.current_org_id', true)::uuid);
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON relay_source_registrations TO ione_app;
    END IF;
END $$;
