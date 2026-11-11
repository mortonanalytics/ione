-- (stream_id, observed_at) is the dedup identity only for events that carry no
-- dedup_key. As a full unique index it also constrained keyed events, whose
-- identity is (stream_id, dedup_key): two keyed events with different keys and
-- one timestamp raised a unique violation, because ON CONFLICT resolves only on
-- the arbiter index it names. Every existing row satisfies the narrower index.
DROP INDEX stream_events_stream_observed_unique;

CREATE UNIQUE INDEX stream_events_stream_observed_unique
    ON stream_events (stream_id, observed_at)
    WHERE dedup_key IS NULL;
