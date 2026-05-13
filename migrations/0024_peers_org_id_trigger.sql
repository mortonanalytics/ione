CREATE OR REPLACE FUNCTION set_peer_org_id_from_issuer()
RETURNS trigger AS $$
BEGIN
    IF NEW.org_id IS NULL THEN
        SELECT org_id INTO NEW.org_id
        FROM trust_issuers
        WHERE id = NEW.issuer_id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS peers_set_org_id ON peers;

CREATE TRIGGER peers_set_org_id
BEFORE INSERT OR UPDATE OF issuer_id, org_id ON peers
FOR EACH ROW
EXECUTE FUNCTION set_peer_org_id_from_issuer();
