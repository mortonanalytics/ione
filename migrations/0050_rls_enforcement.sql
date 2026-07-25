-- Activate the org-isolation row-level-security policies that migrations 0019
-- through 0048 declared but left inert (AC-15 in md/design/identity-broker.md).
--
-- Two of the three reasons those policies never fired are fixed here:
--
--   1. no table declared FORCE ROW LEVEL SECURITY, so PostgreSQL skipped policy
--      evaluation for the table owner — fixed below for all eleven tables;
--   2. the only application role (`ione`) is the table owner and holds both
--      SUPERUSER and BYPASSRLS, so policies could never apply to it — fixed by
--      adding a second, restricted role (`ione_app`) that owns nothing and holds
--      neither attribute.
--
-- The third reason — the application must set `app.current_org_id` per
-- transaction — is fixed in code by `src/rls.rs::org_scoped_tx`.
--
-- NOTE for readers of the earlier migrations: 0019 through 0048 carry header
-- comments describing their RLS policies as inert, and those comments are now
-- out of date. They are deliberately NOT edited — sqlx checksums applied
-- migrations, so amending one breaks `sqlx migrate run` for every existing
-- database. This migration is the authoritative statement of what changed;
-- md/design/identity-broker.md documents the exact coverage boundary, including
-- what is still bypassed under the default `ione` connection.
--
-- `ione_app` is deliberately OPT-IN. `DATABASE_URL`, docker-compose, and CI keep
-- connecting as `ione`, which still bypasses RLS because it is SUPERUSER, so the
-- dev loop and every existing test are unaffected. Only the repository methods
-- listed under "AC-15" in md/design/identity-broker.md set the org context, so
-- `ione_app` is not yet a supported runtime role for the whole binary — it exists
-- so enforcement is provable (tests/rls_enforcement_integration.rs) and so an
-- operator can adopt it once the remaining repositories are migrated.

ALTER TABLE auto_exec_policies           FORCE ROW LEVEL SECURITY;
ALTER TABLE broker_credentials           FORCE ROW LEVEL SECURITY;
ALTER TABLE identity_audit_events        FORCE ROW LEVEL SECURITY;
ALTER TABLE interaction_events           FORCE ROW LEVEL SECURITY;
ALTER TABLE mfa_enrollments              FORCE ROW LEVEL SECURITY;
ALTER TABLE mfa_recovery_codes           FORCE ROW LEVEL SECURITY;
ALTER TABLE peer_catalog_entries         FORCE ROW LEVEL SECURITY;
ALTER TABLE service_account_tokens       FORCE ROW LEVEL SECURITY;
ALTER TABLE workspace_peer_bindings      FORCE ROW LEVEL SECURITY;
ALTER TABLE workspace_peer_credentials   FORCE ROW LEVEL SECURITY;
ALTER TABLE workspace_peer_delegations   FORCE ROW LEVEL SECURITY;

-- The role may already exist: this migration runs against every developer
-- database in a shared cluster, and the role is cluster-scoped, not
-- database-scoped. Create it only when absent, then assert the two attributes
-- that make RLS reachable, so an existing role cannot silently keep BYPASSRLS.
--
-- The bootstrap password is a development default. Operators adopting this role
-- must `ALTER ROLE ione_app PASSWORD '<secret>'` as part of deployment; it is
-- never read from `.env` and never grants more than the DML below.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ione_app') THEN
        CREATE ROLE ione_app LOGIN PASSWORD 'ione_app';
    END IF;
END $$;

ALTER ROLE ione_app NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE NOREPLICATION;

GRANT USAGE ON SCHEMA public TO ione_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO ione_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO ione_app;

-- Tables and sequences added by later migrations are created by the migration
-- role, so the same grants must follow automatically.
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO ione_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO ione_app;
