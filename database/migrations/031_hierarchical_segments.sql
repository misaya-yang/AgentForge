-- Migration: 031_hierarchical_segments.sql
-- Purpose: Add hierarchical indexing support for three-level RAG architecture
-- L1: Document Summary, L2: Section/Chapter, L3: Paragraph

-- Add hierarchical fields to segments table
ALTER TABLE segments ADD COLUMN IF NOT EXISTS level INTEGER DEFAULT 3;
COMMENT ON COLUMN segments.level IS 'Hierarchical level: 1=document, 2=section, 3=paragraph';

-- FIX: Add CHECK constraint for valid level values (1, 2, 3)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_segments_level'
    ) THEN
        ALTER TABLE segments ADD CONSTRAINT chk_segments_level CHECK (level IN (1, 2, 3));
    END IF;
END $$;

-- parent_segment_id should match segment_id type (character varying)
ALTER TABLE segments ADD COLUMN IF NOT EXISTS parent_segment_id VARCHAR(255);
COMMENT ON COLUMN segments.parent_segment_id IS 'Parent segment ID for hierarchical relationships';

-- FIX: Add self-referencing foreign key for hierarchical integrity
-- Note: Using DEFERRABLE to allow batch inserts with temporary violations
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'fk_segments_parent'
    ) THEN
        -- Clean up any orphaned records first
        DELETE FROM segments 
        WHERE parent_segment_id IS NOT NULL 
          AND parent_segment_id NOT IN (SELECT segment_id FROM segments);
        
        ALTER TABLE segments 
        ADD CONSTRAINT fk_segments_parent 
        FOREIGN KEY (parent_segment_id) 
        REFERENCES segments(segment_id) 
        ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED;
    END IF;
END $$;

ALTER TABLE segments ADD COLUMN IF NOT EXISTS summary TEXT;
COMMENT ON COLUMN segments.summary IS 'LLM-generated summary for L1/L2 segments';

-- keywords already exists as jsonb, skip adding as TEXT[]
-- Note: Do not add COMMENT ON COLUMN for existing column to avoid overwriting existing comment

ALTER TABLE segments ADD COLUMN IF NOT EXISTS page_start INTEGER;
COMMENT ON COLUMN segments.page_start IS 'Starting page number (1-indexed)';

ALTER TABLE segments ADD COLUMN IF NOT EXISTS page_end INTEGER;
COMMENT ON COLUMN segments.page_end IS 'Ending page number (1-indexed, inclusive)';

-- FIX: Add CHECK constraint for valid page range
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_segments_page_range'
    ) THEN
        ALTER TABLE segments 
        ADD CONSTRAINT chk_segments_page_range 
        CHECK (page_end IS NULL OR page_start IS NULL OR page_end >= page_start);
    END IF;
END $$;

-- Create index for hierarchical queries
CREATE INDEX IF NOT EXISTS idx_segments_level ON segments(dataset_id, level);
CREATE INDEX IF NOT EXISTS idx_segments_parent ON segments(parent_segment_id) WHERE parent_segment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_segments_document_level ON segments(document_id, level);

-- Add detection_result column to documents for storing auto-detection results
ALTER TABLE documents ADD COLUMN IF NOT EXISTS detection_result JSONB;
COMMENT ON COLUMN documents.detection_result IS 'Result of automatic document type detection';

-- FIX: Add GIN index for detection_result if it will be queried
CREATE INDEX IF NOT EXISTS idx_documents_detection_result ON documents USING GIN (detection_result) 
WHERE detection_result IS NOT NULL;

-- Document summaries table for L1 layer
-- Stores document-level summaries separately for quick lookup
CREATE TABLE IF NOT EXISTS document_summaries (
    document_id VARCHAR(255) PRIMARY KEY REFERENCES documents(document_id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    keywords JSONB DEFAULT '[]'::jsonb,
    topics JSONB DEFAULT '[]'::jsonb,
    vector_id VARCHAR(64),
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

COMMENT ON TABLE document_summaries IS 'Document-level summaries for L1 hierarchical retrieval';

-- Index for keyword search (using jsonb instead of GIN on TEXT[])
CREATE INDEX IF NOT EXISTS idx_document_summaries_keywords ON document_summaries USING GIN (keywords);

-- FIX: Add index for vector_id lookups
CREATE INDEX IF NOT EXISTS idx_document_summaries_vector_id ON document_summaries(vector_id) 
WHERE vector_id IS NOT NULL;

-- FIX: Enhanced trigger function with security settings
CREATE OR REPLACE FUNCTION update_document_summaries_timestamp()
RETURNS TRIGGER 
SECURITY INVOKER
SET search_path = public
AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trigger_document_summaries_updated_at ON document_summaries;
CREATE TRIGGER trigger_document_summaries_updated_at
    BEFORE UPDATE ON document_summaries
    FOR EACH ROW
    EXECUTE FUNCTION update_document_summaries_timestamp();

-- Migration complete

/*
-- Rollback SQL (for reference only - do not run directly):
-- 
-- ALTER TABLE segments DROP CONSTRAINT IF EXISTS chk_segments_level;
-- ALTER TABLE segments DROP CONSTRAINT IF EXISTS fk_segments_parent;
-- ALTER TABLE segments DROP CONSTRAINT IF EXISTS chk_segments_page_range;
-- ALTER TABLE segments DROP COLUMN IF EXISTS level;
-- ALTER TABLE segments DROP COLUMN IF EXISTS parent_segment_id;
-- ALTER TABLE segments DROP COLUMN IF EXISTS summary;
-- ALTER TABLE segments DROP COLUMN IF EXISTS page_start;
-- ALTER TABLE segments DROP COLUMN IF EXISTS page_end;
-- DROP INDEX IF EXISTS idx_documents_detection_result;
-- DROP TABLE IF EXISTS document_summaries;
-- ALTER TABLE documents DROP COLUMN IF EXISTS detection_result;
*/
