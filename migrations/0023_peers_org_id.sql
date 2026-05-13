ALTER TABLE peers ADD COLUMN org_id UUID REFERENCES organizations(id);

UPDATE peers
SET org_id = (SELECT id FROM organizations ORDER BY created_at LIMIT 1)
WHERE org_id IS NULL;

ALTER TABLE peers ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX peers_org ON peers(org_id);
