CREATE TYPE routing_target AS ENUM ('feed','notification','draft','peer');

CREATE TABLE routing_decisions (
    id               UUID           PRIMARY KEY DEFAULT gen_random_uuid(),
    survivor_id      UUID           NOT NULL REFERENCES survivors(id) ON DELETE CASCADE,
    target_kind      routing_target NOT NULL,
    target_ref       JSONB          NOT NULL,
    classifier_model TEXT           NOT NULL,
    rationale        TEXT           NOT NULL,
    created_at       TIMESTAMPTZ    NOT NULL DEFAULT now()
);

CREATE INDEX routing_decisions_survivor ON routing_decisions(survivor_id);
