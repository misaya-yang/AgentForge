-- ============================================================
-- Migration: 008_fix_confluence_sync_tasks
-- Description: Add missing updated_at column to confluence_sync_tasks table
-- Version: 2.3.1
-- ============================================================

SET client_encoding TO 'UTF8';

-- Add missing updated_at column to confluence_sync_tasks
ALTER TABLE confluence_sync_tasks ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Add comment
COMMENT ON COLUMN confluence_sync_tasks.updated_at IS '记录更新时间';

-- ============================================================
-- Migration complete
-- ============================================================
