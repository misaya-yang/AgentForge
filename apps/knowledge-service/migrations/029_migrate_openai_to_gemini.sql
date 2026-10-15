-- Migration 029: Migrate OpenAI embedding provider to Gemini
-- The OpenAI embedding provider has been removed. Existing datasets that used
-- OpenAI embeddings need to be migrated to the Gemini provider.
--
-- IMPORTANT: This migration updates metadata labels AND marks affected datasets
-- for re-indexing. Existing vectors in the vector store are still OpenAI
-- 1536-dimensional embeddings which are incompatible with Gemini 1024-dimensional
-- vectors. Affected datasets MUST be re-indexed before adding new documents.

-- Step 1: Add needs_reindex flag column if it doesn't exist
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS needs_reindex BOOLEAN NOT NULL DEFAULT false;

-- Step 2: Update index_config JSONB and top-level columns together
UPDATE datasets
SET index_config = jsonb_set(
    jsonb_set(
        jsonb_set(
            index_config,
            '{embedding_provider}',
            '"gemini"'
        ),
        '{embedding_model}',
        '"gemini-embedding-001"'
    ),
    '{embedding_dimension}',
    '1024'
),
embedding_provider = 'gemini',
embedding_model = 'gemini-embedding-001',
embedding_dimension = 1024,
needs_reindex = true
WHERE index_config->>'embedding_provider' = 'openai'
   OR embedding_provider = 'openai';

-- Update any default_embedding_provider references in index_config
UPDATE datasets
SET index_config = jsonb_set(
    index_config,
    '{default_embedding_provider}',
    '"gemini"'
)
WHERE index_config->>'default_embedding_provider' = 'openai';
