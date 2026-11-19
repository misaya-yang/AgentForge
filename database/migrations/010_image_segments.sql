-- Migration: 010_image_segments
-- Description: Add support for image segments in knowledge base
-- Date: 2026-01-09

-- Add content_type column to segments table to distinguish text from images
ALTER TABLE segments ADD COLUMN IF NOT EXISTS content_type VARCHAR(50) DEFAULT 'text';

-- Add image-specific columns
ALTER TABLE segments ADD COLUMN IF NOT EXISTS image_url TEXT;  -- Storage URL for images
ALTER TABLE segments ADD COLUMN IF NOT EXISTS image_attachment_id VARCHAR(100);  -- Confluence attachment ID
ALTER TABLE segments ADD COLUMN IF NOT EXISTS image_filename VARCHAR(255);
ALTER TABLE segments ADD COLUMN IF NOT EXISTS image_media_type VARCHAR(100);
ALTER TABLE segments ADD COLUMN IF NOT EXISTS image_file_size INTEGER;

-- Create index for filtering by content type
CREATE INDEX IF NOT EXISTS idx_segments_content_type ON segments(content_type);

-- Create index for finding image segments by attachment ID
CREATE INDEX IF NOT EXISTS idx_segments_image_attachment ON segments(image_attachment_id) WHERE image_attachment_id IS NOT NULL;

-- Add image sync settings to confluence_space_bindings
-- Default to true to enable multimodal RAG by default
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS sync_images BOOLEAN DEFAULT true;
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS image_max_size_bytes INTEGER DEFAULT 3145728;  -- 3 MB

-- Add image tracking to confluence_pages
ALTER TABLE confluence_pages ADD COLUMN IF NOT EXISTS image_count INTEGER DEFAULT 0;
ALTER TABLE confluence_pages ADD COLUMN IF NOT EXISTS images_synced_at TIMESTAMPTZ;

-- Add last_sync_type to track incremental vs full sync
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS last_sync_type VARCHAR(50);

-- Create table for tracking image sync status (optional, for detailed tracking)
CREATE TABLE IF NOT EXISTS confluence_image_sync (
    id BIGSERIAL PRIMARY KEY,
    page_record_id VARCHAR(255) REFERENCES confluence_pages(id) ON DELETE CASCADE,
    attachment_id VARCHAR(100) NOT NULL,
    filename VARCHAR(255) NOT NULL,
    media_type VARCHAR(100),
    file_size INTEGER,
    storage_url TEXT,
    segment_id VARCHAR(255),  -- Reference to segments table (no FK for flexibility)
    status VARCHAR(50) DEFAULT 'pending',  -- pending | synced | error | skipped
    error_message TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    synced_at TIMESTAMPTZ,
    UNIQUE(page_record_id, attachment_id)
);

-- Create indexes for image sync tracking
CREATE INDEX IF NOT EXISTS idx_confluence_image_sync_page ON confluence_image_sync(page_record_id);
CREATE INDEX IF NOT EXISTS idx_confluence_image_sync_status ON confluence_image_sync(status);

-- Comments for documentation
COMMENT ON COLUMN segments.content_type IS 'Type of segment content: text or image';
COMMENT ON COLUMN segments.image_url IS 'Storage URL for image content (S3/OSS/local)';
COMMENT ON COLUMN segments.image_attachment_id IS 'Confluence attachment ID for images';
COMMENT ON COLUMN confluence_space_bindings.sync_images IS 'Whether to sync and embed images from Confluence pages';
COMMENT ON COLUMN confluence_space_bindings.image_max_size_bytes IS 'Maximum image size to sync (default 3MB for DashScope limit)';
