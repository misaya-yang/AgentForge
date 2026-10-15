-- Migration 048: Add composite indexes for common query patterns
-- Identified in KB-Deep-Analysis-2026-04-09

-- segments: common filter combination (dataset + enabled + content_type)
CREATE INDEX IF NOT EXISTS idx_segments_dataset_enabled_type
ON segments (dataset_id, enabled, content_type);

-- documents: compound index for status filtering within dataset
CREATE INDEX IF NOT EXISTS idx_documents_dataset_status
ON documents (dataset_id, status);

-- segment_images: reverse lookup by image segment
CREATE INDEX IF NOT EXISTS idx_segment_images_image_id
ON segment_images (image_segment_id);
