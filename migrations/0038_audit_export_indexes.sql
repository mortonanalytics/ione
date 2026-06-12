-- Audit & T&E event export: filter/keyset indexes.
-- id DESC included so the keyset ORDER BY (created_at DESC, id DESC) and the
-- cursor predicate (created_at, id) < ($1, $2) are fully index-served.
CREATE INDEX audit_events_ws_actor_kind_created ON audit_events (workspace_id, actor_kind, created_at DESC, id DESC);
CREATE INDEX audit_events_ws_verb_created       ON audit_events (workspace_id, verb, created_at DESC, id DESC);
CREATE INDEX audit_events_ws_actor_ref_created  ON audit_events (workspace_id, actor_ref, created_at DESC, id DESC);
CREATE INDEX pipeline_events_ws_stage_time      ON pipeline_events (workspace_id, stage, occurred_at DESC);
