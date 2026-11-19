-- Add content_hash field to segments table for incremental updates
-- This allows us to detect unchanged segments and skip re-embedding

-- Add content_hash column
ALTER TABLE segments ADD COLUMN IF NOT EXISTS content_hash VARCHAR(64);

-- Create index for efficient hash lookups during incremental updates
CREATE INDEX IF NOT EXISTS idx_segments_content_hash ON segments(document_id, content_hash);

-- Backfill existing segments with MD5 hash of their text content
-- Using MD5 for consistency with the chunking module's hash_id
UPDATE segments
SET content_hash = MD5(text)
WHERE content_hash IS NULL AND text IS NOT NULL AND text != '';

-- Add comment explaining the field
COMMENT ON COLUMN segments.content_hash IS 'MD5 hash of segment text content for incremental update detection';
