-- Migration 088 — tenant-configurable model capability profiles
--
-- The columns are additive and safe for old binaries.  Catalog defaults and
-- operator overrides are stored separately so provider sync cannot erase a
-- tenant's explicit choices.

ALTER TABLE llm_models
    ADD COLUMN IF NOT EXISTS catalog_capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS capability_overrides JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS capability_revision BIGINT NOT NULL DEFAULT 1;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'llm_models_catalog_capabilities_object'
    ) THEN
        ALTER TABLE llm_models
            ADD CONSTRAINT llm_models_catalog_capabilities_object
            CHECK (jsonb_typeof(catalog_capabilities) = 'object');
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'llm_models_capability_overrides_object'
    ) THEN
        ALTER TABLE llm_models
            ADD CONSTRAINT llm_models_capability_overrides_object
            CHECK (jsonb_typeof(capability_overrides) = 'object');
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'llm_models_capability_revision_positive'
    ) THEN
        ALTER TABLE llm_models
            ADD CONSTRAINT llm_models_capability_revision_positive
            CHECK (capability_revision > 0);
    END IF;
END $$;

COMMENT ON COLUMN llm_models.catalog_capabilities IS
    'Versioned provider catalog capability profile; provider sync may update it.';
COMMENT ON COLUMN llm_models.capability_overrides IS
    'Tenant administrator capability overrides; provider sync must preserve it.';
COMMENT ON COLUMN llm_models.capability_revision IS
    'Optimistic concurrency revision for effective capability changes.';
