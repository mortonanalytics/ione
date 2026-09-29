ALTER TABLE relay_peer_dataset_mappings
    ADD COLUMN owner_tenant_id uuid,
    ADD COLUMN owner_workspace_id uuid,
    ADD COLUMN owner_deployment_id uuid,
    ADD COLUMN peer_url text,
    ADD CONSTRAINT peer_dataset_binding_pins_complete CHECK (
        (owner_tenant_id IS NULL AND owner_workspace_id IS NULL AND owner_deployment_id IS NULL AND peer_url IS NULL)
        OR (owner_tenant_id IS NOT NULL AND owner_workspace_id IS NOT NULL AND owner_deployment_id IS NOT NULL AND peer_url IS NOT NULL)
    );
CREATE FUNCTION preserve_peer_dataset_binding_pins() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.owner_tenant_id IS NOT NULL AND
       (NEW.owner_tenant_id,NEW.owner_workspace_id,NEW.owner_deployment_id,NEW.peer_url)
       IS DISTINCT FROM
       (OLD.owner_tenant_id,OLD.owner_workspace_id,OLD.owner_deployment_id,OLD.peer_url) THEN
        RAISE EXCEPTION 'peer dataset binding pins are immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER preserve_peer_dataset_binding_pins BEFORE UPDATE ON relay_peer_dataset_mappings
    FOR EACH ROW EXECUTE FUNCTION preserve_peer_dataset_binding_pins();
