-- Migration: 014_multimodal_chunks
-- Description: Add support for image-text chunk association (Dify-style Smart Attachment Handling)
-- Date: 2026-01-12
-- Part of P3: Multimodal RAG Full-Chain Optimization

-- ============================================================
-- Image-Chunk Association Table
-- ============================================================
-- Links image segments to text segments for multimodal retrieval.
-- Each text chunk can have up to 10 associated images (Dify pattern).
-- Images are associated based on proximity in source document.

CREATE TABLE IF NOT EXISTS segment_images (
    id BIGSERIAL PRIMARY KEY,

    -- The text segment that images are associated with
    segment_id VARCHAR(255) NOT NULL REFERENCES segments(segment_id) ON DELETE CASCADE,

    -- The image segment being associated
    image_segment_id VARCHAR(255) NOT NULL,

    -- Position of image within the text chunk context (0-indexed)
    position INTEGER NOT NULL DEFAULT 0,

    -- Proximity score [0,1] indicating how closely related the image is to the text
    -- 1.0 = directly embedded in text, 0.5 = nearby in document, etc.
    proximity_score FLOAT NOT NULL DEFAULT 1.0 CHECK (proximity_score >= 0 AND proximity_score <= 1),

    -- Original position information for debugging/analysis
    char_offset INTEGER DEFAULT 0,      -- Character offset in source document
    page_number INTEGER DEFAULT NULL,   -- Page number for PDF/multi-page docs

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Ensure unique association
    UNIQUE(segment_id, image_segment_id)
);

-- ============================================================
-- Indexes for segment_images
-- ============================================================

-- Primary lookup: find all images for a text segment
CREATE INDEX IF NOT EXISTS idx_segment_images_segment
ON segment_images(segment_id);

-- Reverse lookup: find which text segments an image belongs to
CREATE INDEX IF NOT EXISTS idx_segment_images_image
ON segment_images(image_segment_id);

-- Query by proximity for ranking
CREATE INDEX IF NOT EXISTS idx_segment_images_proximity
ON segment_images(segment_id, proximity_score DESC);

-- ============================================================
-- Add multimodal flags to segments table
-- ============================================================

-- Flag indicating whether a text segment has associated images
ALTER TABLE segments
ADD COLUMN IF NOT EXISTS has_images BOOLEAN NOT NULL DEFAULT FALSE;

-- Count of associated images (for quick filtering)
ALTER TABLE segments
ADD COLUMN IF NOT EXISTS image_count INTEGER NOT NULL DEFAULT 0 CHECK (image_count >= 0);

-- VLM description for image segments (moved from text field for clarity)
ALTER TABLE segments
ADD COLUMN IF NOT EXISTS vlm_description TEXT DEFAULT NULL;

-- Create index for filtering segments with images
CREATE INDEX IF NOT EXISTS idx_segments_has_images
ON segments(has_images) WHERE has_images = TRUE;

-- ============================================================
-- Comments for documentation
-- ============================================================

COMMENT ON TABLE segment_images IS
'Links image segments to text segments for multimodal RAG retrieval. Part of P3 optimization.';

COMMENT ON COLUMN segment_images.segment_id IS
'Text segment that images are associated with (references segments.segment_id)';

COMMENT ON COLUMN segment_images.image_segment_id IS
'Image segment being associated (references segments.segment_id where content_type=image)';

COMMENT ON COLUMN segment_images.proximity_score IS
'How closely related the image is to the text [0,1]. 1.0 = embedded inline, 0.5 = nearby';

COMMENT ON COLUMN segment_images.char_offset IS
'Character offset in source document where image was found';

COMMENT ON COLUMN segment_images.page_number IS
'Page number in PDF/multi-page documents';

COMMENT ON COLUMN segments.has_images IS
'Quick flag indicating this text segment has associated images for multimodal retrieval';

COMMENT ON COLUMN segments.image_count IS
'Number of associated images (max 10 per Dify pattern)';

COMMENT ON COLUMN segments.vlm_description IS
'VLM-generated description for image segments';

-- ============================================================
-- Helper function to update image counts (optional trigger)
-- ============================================================

-- Function to update has_images and image_count on segments
CREATE OR REPLACE FUNCTION update_segment_image_counts()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE segments
        SET has_images = TRUE,
            image_count = (SELECT COUNT(*) FROM segment_images WHERE segment_id = NEW.segment_id)
        WHERE segment_id = NEW.segment_id;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE segments
        SET image_count = (SELECT COUNT(*) FROM segment_images WHERE segment_id = OLD.segment_id),
            has_images = (SELECT COUNT(*) > 0 FROM segment_images WHERE segment_id = OLD.segment_id)
        WHERE segment_id = OLD.segment_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Create trigger to automatically maintain counts
DROP TRIGGER IF EXISTS trg_update_segment_image_counts ON segment_images;
CREATE TRIGGER trg_update_segment_image_counts
AFTER INSERT OR DELETE ON segment_images
FOR EACH ROW
EXECUTE FUNCTION update_segment_image_counts();
