-- Scale-hardening for interaction_events (design §"Indexes (v1)" item 5 +
-- write-path autovacuum tuning). Additive; no data change. Separate from 0045
-- so environments that already applied 0045 do not see a checksum mismatch.

-- Org-level analytics index ("build now, query later"): cheaper to add now than
-- CREATE INDEX CONCURRENTLY on a large table once org-scoped endpoints land.
-- (workspace_id queries are served by 0045's indexes; this covers org rollups.)
CREATE INDEX IF NOT EXISTS interaction_events_org_recorded_idx
    ON interaction_events (org_id, recorded_at DESC, id DESC);

-- High-insert append-only table: vacuum dead tuples aggressively so index bloat
-- stays bounded under the batch-writer load.
ALTER TABLE interaction_events SET (autovacuum_vacuum_scale_factor = 0.01);
