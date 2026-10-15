-- 084: Connector catalog productization — mode column.
-- Connector definitions gain a mode ('live' | 'ingest' | 'both') describing
-- whether the provider is used for live assistant tools, content ingestion
-- into the knowledge base, or both. Forward-only and additive; the default
-- 'live' preserves existing behavior, and providers already flagged
-- supports_sync are backfilled to 'both' so ingest-capable definitions keep
-- their prior capability without operator action.

BEGIN;

ALTER TABLE connector_configs
    ADD COLUMN IF NOT EXISTS mode VARCHAR(16) NOT NULL DEFAULT 'live';

DO $$
BEGIN
    ALTER TABLE connector_configs
        ADD CONSTRAINT connector_configs_mode_check
        CHECK (mode IN ('live', 'ingest', 'both'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END
$$;

UPDATE connector_configs
SET mode = 'both'
WHERE supports_sync = TRUE
  AND mode = 'live';

COMMIT;
