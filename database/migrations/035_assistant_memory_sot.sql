-- Migration: 035_assistant_memory_sot.sql
-- Goal: Add markdown source-of-truth memory tables + chunk index tables

BEGIN;

CREATE TABLE IF NOT EXISTS assistant_memory_sources (
    source_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    source_path TEXT NOT NULL,
    source_type VARCHAR(32) NOT NULL DEFAULT 'daily',
    content_hash VARCHAR(64) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, user_id, source_path)
);

CREATE INDEX IF NOT EXISTS idx_assistant_memory_sources_tenant_user
    ON assistant_memory_sources(tenant_id, user_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS assistant_memory_chunks (
    chunk_id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES assistant_memory_sources(source_id) ON DELETE CASCADE,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    chunk_index INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    content TEXT NOT NULL,
    token_estimate INTEGER NOT NULL DEFAULT 0,
    text_search tsvector,
    vector_id VARCHAR(128),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (source_id, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_assistant_memory_chunks_tenant_user
    ON assistant_memory_chunks(tenant_id, user_id, chunk_index);

CREATE INDEX IF NOT EXISTS idx_assistant_memory_chunks_text_search
    ON assistant_memory_chunks USING GIN (text_search);

CREATE OR REPLACE FUNCTION update_assistant_memory_chunks_text_search()
RETURNS TRIGGER AS $$
BEGIN
    NEW.text_search := to_tsvector('simple', COALESCE(NEW.content, ''));
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_assistant_memory_chunks_text_search ON assistant_memory_chunks;
CREATE TRIGGER trg_assistant_memory_chunks_text_search
    BEFORE INSERT OR UPDATE OF content ON assistant_memory_chunks
    FOR EACH ROW
    EXECUTE FUNCTION update_assistant_memory_chunks_text_search();

CREATE TABLE IF NOT EXISTS assistant_memory_reflections (
    reflection_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    reflection_date DATE NOT NULL,
    summary TEXT NOT NULL,
    extracted_facts JSONB NOT NULL DEFAULT '[]'::jsonb,
    source_paths JSONB NOT NULL DEFAULT '[]'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, user_id, reflection_date)
);

CREATE INDEX IF NOT EXISTS idx_assistant_memory_reflections_tenant_user_date
    ON assistant_memory_reflections(tenant_id, user_id, reflection_date DESC);

COMMIT;
