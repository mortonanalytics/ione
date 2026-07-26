-- Distinguish "the operator authorized this peer with an empty tool allowlist"
-- from "no allowlist was ever configured".
--
-- `tool_allowlist` is `JSONB NOT NULL DEFAULT '[]'`, so `[]` is ambiguous: it is
-- both the column default for a peer that never went through
-- `POST /api/v1/peers/:id/authorize`, and the legitimate result of authorizing a
-- peer with zero tools. Enforcement on the federated invocation path therefore
-- could not fail closed on `[]` without also denying every peer whose row was
-- created directly (which is how the demo seeder and most fixtures create them).
--
-- With this flag the two cases separate: `set_allowlist` is the only writer and
-- sets it true, so `configured AND empty` means "authorized to invoke nothing"
-- and is denied, while `NOT configured` keeps the pre-existing fall-through.
--
-- Backfill deliberately leaves existing rows false. Marking them configured
-- would silently deny every already-federated peer whose allowlist is `[]`; the
-- conservative direction is to preserve current behaviour and let the next
-- authorize call set the flag.
ALTER TABLE peers
    ADD COLUMN tool_allowlist_configured BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN peers.tool_allowlist_configured IS
    'True once POST /api/v1/peers/:id/authorize has written tool_allowlist. '
    'Disambiguates an authorized-but-empty allowlist (deny all tools) from a '
    'peer that was never authorized through that route (fall through).';
