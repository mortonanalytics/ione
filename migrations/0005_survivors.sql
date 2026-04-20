CREATE TYPE critic_verdict AS ENUM ('survive','reject','defer');

CREATE TABLE survivors (
    id                 UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    signal_id          UUID          NOT NULL UNIQUE REFERENCES signals(id) ON DELETE CASCADE,
    critic_model       TEXT          NOT NULL,
    verdict            critic_verdict NOT NULL,
    rationale          TEXT          NOT NULL,
    confidence         REAL          NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    chain_of_reasoning JSONB         NOT NULL DEFAULT '[]'::jsonb,
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT now()
);

CREATE INDEX survivors_signal_id ON survivors(signal_id);
