-- Migration: 027_fix_usage_daily_aggregates_constraint.sql
-- Description: Fix ON CONFLICT issue by adding proper UNIQUE CONSTRAINT
-- Date: 2026-01-23
--
-- Problem: The existing unique index uses COALESCE expressions which don't work
-- with ON CONFLICT (column_list) syntax. PostgreSQL requires exact match.
--
-- Solution: Add a proper UNIQUE CONSTRAINT on the columns (with NULLs converted
-- to empty strings in the application layer).

-- ============================================================================
-- Step 1: Drop the expression-based unique index
-- ============================================================================
DROP INDEX IF EXISTS idx_usage_daily_unique;

-- ============================================================================
-- Step 2: Add proper UNIQUE CONSTRAINT
-- Note: The application code will use COALESCE to ensure no NULLs are inserted
-- ============================================================================
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'uq_usage_daily_aggregates_dimensions'
    ) THEN
        ALTER TABLE usage_daily_aggregates
        ADD CONSTRAINT uq_usage_daily_aggregates_dimensions
        UNIQUE (tenant_id, user_id, model, assistant_id, service_id, date);
    END IF;
END $$;

-- ============================================================================
-- Step 3: Update existing NULL values to empty strings (if any)
-- This ensures data consistency before the constraint takes effect
-- ============================================================================
UPDATE usage_daily_aggregates
SET
    user_id = COALESCE(user_id, ''),
    model = COALESCE(model, ''),
    assistant_id = COALESCE(assistant_id, ''),
    service_id = COALESCE(service_id, '')
WHERE
    user_id IS NULL
    OR model IS NULL
    OR assistant_id IS NULL
    OR service_id IS NULL;

-- ============================================================================
-- Step 4: Add NOT NULL constraints to prevent future NULLs
-- ============================================================================
ALTER TABLE usage_daily_aggregates
ALTER COLUMN user_id SET NOT NULL,
ALTER COLUMN user_id SET DEFAULT '',
ALTER COLUMN model SET NOT NULL,
ALTER COLUMN model SET DEFAULT '',
ALTER COLUMN assistant_id SET NOT NULL,
ALTER COLUMN assistant_id SET DEFAULT '',
ALTER COLUMN service_id SET NOT NULL,
ALTER COLUMN service_id SET DEFAULT '';

-- ============================================================================
-- Same fix for usage_hourly_aggregates table
-- ============================================================================
DROP INDEX IF EXISTS idx_usage_hourly_unique;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'uq_usage_hourly_aggregates_dimensions'
    ) THEN
        ALTER TABLE usage_hourly_aggregates
        ADD CONSTRAINT uq_usage_hourly_aggregates_dimensions
        UNIQUE (tenant_id, user_id, model, assistant_id, service_id, bucket_start);
    END IF;
END $$;

UPDATE usage_hourly_aggregates
SET
    user_id = COALESCE(user_id, ''),
    model = COALESCE(model, ''),
    assistant_id = COALESCE(assistant_id, ''),
    service_id = COALESCE(service_id, '')
WHERE
    user_id IS NULL
    OR model IS NULL
    OR assistant_id IS NULL
    OR service_id IS NULL;

ALTER TABLE usage_hourly_aggregates
ALTER COLUMN user_id SET NOT NULL,
ALTER COLUMN user_id SET DEFAULT '',
ALTER COLUMN model SET NOT NULL,
ALTER COLUMN model SET DEFAULT '',
ALTER COLUMN assistant_id SET NOT NULL,
ALTER COLUMN assistant_id SET DEFAULT '',
ALTER COLUMN service_id SET NOT NULL,
ALTER COLUMN service_id SET DEFAULT '';
