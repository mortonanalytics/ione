-- Federated catalog index: one lexically-searchable, org-scoped row per peer
-- tool/resource. Maintained off refresh_manifest_if_changed (Slice 1).

CREATE TYPE catalog_entry_kind AS ENUM ('tool', 'resource');

-- array_to_string is only STABLE, which a generated column forbids. Wrapping it
-- (text[] + constant delimiter, no null-string → effectively immutable) lets the
-- generated tsvector reference array columns.
CREATE OR REPLACE FUNCTION pce_array_to_text(arr text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
    $$ SELECT coalesce(array_to_string(arr, ' '), '') $$;

CREATE TABLE peer_catalog_entries (
    id                 UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id             UUID         NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    peer_id            UUID         NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
    kind               catalog_entry_kind NOT NULL,
    namespaced_name    TEXT         NOT NULL,          -- '<peer.tool_prefix>:<raw_name>' (invocation form)
    raw_name           TEXT         NOT NULL,
    description        TEXT         NOT NULL DEFAULT '',
    sample_queries     TEXT[]       NOT NULL DEFAULT '{}',
    schema_field_names TEXT[]       NOT NULL DEFAULT '{}',
    content_hash       TEXT         NOT NULL,
    embedding          vector(768),                    -- reserved, unused in v1 (hybrid-v2 footprint)
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- `'english'::regconfig` selects the IMMUTABLE to_tsvector overload, required
    -- for a generated column (the bare-string overload is only STABLE).
    tsv tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('english'::regconfig, coalesce(raw_name, '')), 'A') ||
        setweight(to_tsvector('english'::regconfig, pce_array_to_text(sample_queries)), 'B') ||
        setweight(to_tsvector('english'::regconfig, coalesce(description, '')), 'C') ||
        setweight(to_tsvector('english'::regconfig, pce_array_to_text(schema_field_names)), 'D')
    ) STORED,
    CONSTRAINT pce_unique_entry UNIQUE (org_id, peer_id, namespaced_name)
);

CREATE INDEX pce_tsv_gin ON peer_catalog_entries USING gin (tsv);
CREATE INDEX pce_trgm_gin ON peer_catalog_entries USING gin ((raw_name || ' ' || description) gin_trgm_ops);
CREATE INDEX pce_org_peer ON peer_catalog_entries (org_id, peer_id);

CREATE OR REPLACE FUNCTION pce_touch_updated_at() RETURNS trigger AS $$
BEGIN NEW.updated_at := now(); RETURN NEW; END $$ LANGUAGE plpgsql;

CREATE TRIGGER pce_touch_updated_at_trg
BEFORE UPDATE ON peer_catalog_entries
FOR EACH ROW EXECUTE FUNCTION pce_touch_updated_at();

-- Org-isolation RLS, consistent with the other org-scoped tables. Inert today
-- (app.current_org_id is never set); the application WHERE org_id=$1 is the
-- real guard (FCS-H1).
ALTER TABLE peer_catalog_entries ENABLE ROW LEVEL SECURITY;
CREATE POLICY pce_org_isolation ON peer_catalog_entries
    USING (org_id = current_setting('app.current_org_id', true)::uuid);
