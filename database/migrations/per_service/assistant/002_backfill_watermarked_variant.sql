-- =============================================================================
-- Phase-2 image redesign — fix-up for pre-existing watermarked rows.
-- =============================================================================
-- Problem: 001_image_session_artifacts.sql ALTER ADD COLUMN variant
-- DEFAULT 'raw' fills every existing row with variant='raw'. But pre-mig
-- the route stored watermarked artifacts as separate rows with
-- ``source = 'image_generation_watermarked'`` (alongside the unwatermarked
-- raw row with ``source = 'image_generation'``). Those watermarked rows
-- now claim to be raw — if a caller forwards their old artifact_id as a
-- ``reference_artifact_id`` after the upgrade, the new ``find_variant``
-- treats them as raw and feeds watermarked bytes into Gemini for the
-- next-turn edit. That violates the "next turn always sees raw" contract.
--
-- Fix: relabel watermarked rows as variant='display'. Idempotent — a row
-- that's already 'display' is left alone.
--
-- Related side-effect: pre-mig watermarked rows have no
-- ``parent_artifact_id`` (the linkage didn't exist), so after this fix-up
-- they're orphan display rows. ``find_variant(<display_id>, 'raw')``
-- returns None for orphans (see artifact_storage.py:447) → caller gets 404
-- → can't smuggle watermarked bytes into editing. Exactly the safe
-- behavior we want.
-- =============================================================================

UPDATE assistant.artifacts
SET variant = 'display'
WHERE source = 'image_generation_watermarked'
  AND variant = 'raw';
