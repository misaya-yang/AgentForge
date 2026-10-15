-- ============================================================
-- Migration: 016_confluence_multi_root_pages
-- Description: Add root_page_ids/root_page_titles array columns for multi-select support,
--              and sync_images/image_max_size_bytes for image sync configuration
-- Version: 2.4.0
-- ============================================================

SET client_encoding TO 'UTF8';

-- Add root_page_ids array column to confluence_space_bindings
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS root_page_ids TEXT[] DEFAULT '{}';

-- Add root_page_titles array column to confluence_space_bindings
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS root_page_titles TEXT[] DEFAULT '{}';

-- Add image sync configuration columns
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS sync_images BOOLEAN DEFAULT true;
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS image_max_size_bytes INTEGER DEFAULT 3145728;

-- Add comments
COMMENT ON COLUMN confluence_space_bindings.root_page_ids IS '根页面 ID 列表（支持多选），如果指定则只同步这些页面及其子页面';
COMMENT ON COLUMN confluence_space_bindings.root_page_titles IS '根页面标题列表';
COMMENT ON COLUMN confluence_space_bindings.sync_images IS '是否同步页面中的图片（默认开启）';
COMMENT ON COLUMN confluence_space_bindings.image_max_size_bytes IS '最大图片大小（字节），默认 3MB';

-- Migrate existing single root_page_id to root_page_ids array
UPDATE confluence_space_bindings
SET root_page_ids = ARRAY[root_page_id]
WHERE root_page_id IS NOT NULL AND (root_page_ids IS NULL OR root_page_ids = '{}');

-- Migrate existing single root_page_title to root_page_titles array
UPDATE confluence_space_bindings
SET root_page_titles = ARRAY[root_page_title]
WHERE root_page_title IS NOT NULL AND (root_page_titles IS NULL OR root_page_titles = '{}');

-- ============================================================
-- Migration complete
-- ============================================================
