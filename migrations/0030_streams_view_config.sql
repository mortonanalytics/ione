ALTER TABLE streams ADD COLUMN view_config JSONB;
CREATE INDEX streams_view_config_present ON streams (id) WHERE view_config IS NOT NULL;
COMMENT ON COLUMN streams.view_config IS 'Optional per-stream geometry + style mapping for the /event-layers endpoint. See md/design/event-point-layer.md. NULL = stream is not rendered as a point layer.';
