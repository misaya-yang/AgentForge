-- Migration: 059_llm_provider_metadata.sql
-- Goal: store non-secret provider runtime settings such as Vertex project/location.

BEGIN;

ALTER TABLE llm_providers
    ADD COLUMN IF NOT EXISTS metadata JSONB NOT NULL DEFAULT '{}'::jsonb;

COMMIT;
