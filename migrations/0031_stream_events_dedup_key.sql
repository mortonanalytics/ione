ALTER TABLE stream_events ADD COLUMN dedup_key TEXT;

CREATE UNIQUE INDEX stream_events_stream_dedup_key
    ON stream_events (stream_id, dedup_key)
    WHERE dedup_key IS NOT NULL;
