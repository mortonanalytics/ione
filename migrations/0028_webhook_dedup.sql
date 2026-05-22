CREATE TABLE webhook_events_seen (
  event_id    TEXT NOT NULL,
  peer_id     UUID NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
  received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (event_id, peer_id)
);

CREATE INDEX webhook_events_seen_received ON webhook_events_seen (received_at);
