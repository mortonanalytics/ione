-- Allow multiple roles with the same name at different CoC levels in a workspace.
-- Prior constraint only allowed one role per (workspace_id, name).
-- This change permits, e.g., a "member" role at coc_level=0 (bootstrap seed) and
-- a "member" role at coc_level=1 (test role) to coexist.

ALTER TABLE roles
    DROP CONSTRAINT roles_workspace_id_name_key,
    ADD CONSTRAINT roles_workspace_id_name_coc_level_key UNIQUE (workspace_id, name, coc_level);
