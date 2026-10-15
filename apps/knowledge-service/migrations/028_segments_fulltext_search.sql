-- Migration 028: Add Full-Text Search (FTS) support for segments table
-- Replaces ILIKE sequential scan with GIN-indexed tsvector for BM25 candidate retrieval
-- Performance: O(N) sequential scan → O(log N) index lookup

-- Step 1: Add tsvector column for full-text search
ALTER TABLE segments ADD COLUMN IF NOT EXISTS text_search tsvector;

-- Step 2: Create GIN index on tsvector column for fast full-text search
CREATE INDEX IF NOT EXISTS idx_segments_text_search ON segments USING GIN (text_search);

-- Step 3: Backfill existing rows with tsvector data in batches
-- Use 'simple' config (no stemming/stopwords) to support multilingual content (Chinese, Arabic, English)
-- Batched: processes 5000 rows at a time to avoid long locks and WAL bloat.
-- Safe to re-run: only updates rows where text_search IS NULL.
DO $$
DECLARE
  batch_size INT := 5000;
  updated INT;
BEGIN
  LOOP
    UPDATE segments
    SET text_search = to_tsvector('simple', COALESCE(text, ''))
    WHERE segment_id IN (
      SELECT segment_id FROM segments
      WHERE text_search IS NULL
      LIMIT batch_size
      FOR UPDATE SKIP LOCKED
    );
    GET DIAGNOSTICS updated = ROW_COUNT;
    EXIT WHEN updated = 0;
    PERFORM pg_sleep(0.1);  -- yield to other transactions
    RAISE NOTICE 'FTS backfill: updated % rows', updated;
  END LOOP;
END $$;

-- Step 4: Create trigger to auto-update tsvector on INSERT/UPDATE
CREATE OR REPLACE FUNCTION segments_text_search_update() RETURNS trigger AS $$
BEGIN
    NEW.text_search := to_tsvector('simple', COALESCE(NEW.text, ''));
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_segments_text_search ON segments;
CREATE TRIGGER trg_segments_text_search
    BEFORE INSERT OR UPDATE OF text ON segments
    FOR EACH ROW
    EXECUTE FUNCTION segments_text_search_update();
