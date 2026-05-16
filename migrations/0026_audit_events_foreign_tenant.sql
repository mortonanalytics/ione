ALTER TABLE approvals    ADD COLUMN foreign_tenant_id TEXT NULL;
ALTER TABLE audit_events ADD COLUMN foreign_tenant_id TEXT NULL;
