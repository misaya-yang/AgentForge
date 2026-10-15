-- Migration 015: Add foreign key constraint to segment_images.image_segment_id
--
-- This migration adds a foreign key constraint from segment_images.image_segment_id
-- to segments.segment_id to prevent orphan association records.
--
-- The constraint uses ON DELETE CASCADE so when an image segment is deleted,
-- its associations are also removed.

-- First, remove any orphan records that would violate the constraint
DELETE FROM segment_images
WHERE image_segment_id NOT IN (SELECT segment_id FROM segments);

-- Add the foreign key constraint (idempotent: skip if already exists)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'fk_segment_images_image_segment'
    ) THEN
        ALTER TABLE segment_images
        ADD CONSTRAINT fk_segment_images_image_segment
        FOREIGN KEY (image_segment_id) REFERENCES segments(segment_id) ON DELETE CASCADE;
    END IF;
END $$;

-- Add a comment explaining the constraint
COMMENT ON CONSTRAINT fk_segment_images_image_segment ON segment_images
IS 'Ensures image_segment_id references a valid segment. Cascades deletes.';
