-- Idempotency constraints for declarative provisioning (design Slice 2).
-- These are the ON CONFLICT targets Slice 3's upserts bind to, and are
-- generally-correct hardening (duplicate names per scope are a latent bug).
-- Each ADD CONSTRAINT fails if duplicates exist; dev/test DBs from bootstrap
-- have none. Reversible via DROP CONSTRAINT.

ALTER TABLE workspaces ADD CONSTRAINT workspaces_org_id_name_key UNIQUE (org_id, name);
ALTER TABLE connectors ADD CONSTRAINT connectors_workspace_id_name_key UNIQUE (workspace_id, name);
ALTER TABLE peers ADD CONSTRAINT peers_org_id_name_key UNIQUE (org_id, name);
