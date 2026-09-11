ALTER TABLE workspace_relay_mappings ADD COLUMN peer_dataset_import boolean NOT NULL DEFAULT false;
CREATE FUNCTION preserve_peer_dataset_mapping_kind() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.peer_dataset_import AND NOT NEW.peer_dataset_import THEN
        RAISE EXCEPTION 'peer dataset mapping kind is immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER preserve_peer_dataset_mapping_kind BEFORE UPDATE ON workspace_relay_mappings
    FOR EACH ROW EXECUTE FUNCTION preserve_peer_dataset_mapping_kind();
CREATE TABLE relay_peer_dataset_mappings (
    mapping_id uuid PRIMARY KEY REFERENCES workspace_relay_mappings(id) ON DELETE CASCADE,
    org_id uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    peer_id uuid NOT NULL,
    binding_id uuid NOT NULL,
    grant_id uuid NOT NULL
);
ALTER TABLE relay_peer_dataset_mappings ENABLE ROW LEVEL SECURITY;
ALTER TABLE relay_peer_dataset_mappings FORCE ROW LEVEL SECURITY;
CREATE POLICY relay_peer_dataset_mappings_org ON relay_peer_dataset_mappings
    USING (org_id = current_setting('app.current_org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('app.current_org_id', true)::uuid);
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        GRANT SELECT,INSERT,UPDATE,DELETE ON relay_peer_dataset_mappings TO ione_app;
    END IF;
END $$;
