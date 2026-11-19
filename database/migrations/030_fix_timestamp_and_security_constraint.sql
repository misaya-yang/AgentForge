-- Migration: 030_fix_timestamp_and_security_constraint.sql
-- Description: Fix TIMESTAMP/TIMESTAMPTZ inconsistency and security_event COALESCE index
-- Date: 2026-01-29
--
-- WARNING: ALTER COLUMN ... TYPE requires ACCESS EXCLUSIVE lock and rewrites the table.
-- For large tables (especially usage_records), this will block all reads/writes during
-- the rewrite. Run during a maintenance window if usage_records has millions of rows.
--
-- Issues fixed:
-- 1. Tables created in migrations 016-018 use TIMESTAMP (no timezone) while the rest
--    of the schema uses TIMESTAMPTZ. This can cause timezone-related bugs in cross-table
--    queries. Fix: ALTER all TIMESTAMP columns to TIMESTAMPTZ.
-- 2. security_event_daily_aggregates uses COALESCE expression-based unique index,
--    which is incompatible with ON CONFLICT (column_list) syntax.
--    Fix: Same approach as migration 027 for usage tables.

-- ============================================================================
-- Part 1: Fix TIMESTAMP → TIMESTAMPTZ for usage_records
-- ============================================================================
ALTER TABLE usage_records
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 2: Fix TIMESTAMP → TIMESTAMPTZ for usage_daily_aggregates
-- ============================================================================
ALTER TABLE usage_daily_aggregates
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 3: Fix TIMESTAMP → TIMESTAMPTZ for usage_hourly_aggregates
-- ============================================================================
ALTER TABLE usage_hourly_aggregates
    ALTER COLUMN bucket_start TYPE TIMESTAMPTZ USING bucket_start AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 4: Fix TIMESTAMP → TIMESTAMPTZ for user_quotas
-- ============================================================================
ALTER TABLE user_quotas
    ALTER COLUMN daily_reset_at TYPE TIMESTAMPTZ USING daily_reset_at AT TIME ZONE 'UTC',
    ALTER COLUMN monthly_reset_at TYPE TIMESTAMPTZ USING monthly_reset_at AT TIME ZONE 'UTC',
    ALTER COLUMN blocked_at TYPE TIMESTAMPTZ USING blocked_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 5: Fix TIMESTAMP → TIMESTAMPTZ for model_pricing
-- ============================================================================
ALTER TABLE model_pricing
    ALTER COLUMN effective_from TYPE TIMESTAMPTZ USING effective_from AT TIME ZONE 'UTC',
    ALTER COLUMN effective_to TYPE TIMESTAMPTZ USING effective_to AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 6: Fix TIMESTAMP → TIMESTAMPTZ for quota_alerts
-- ============================================================================
ALTER TABLE quota_alerts
    ALTER COLUMN acknowledged_at TYPE TIMESTAMPTZ USING acknowledged_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 7: Fix TIMESTAMP → TIMESTAMPTZ for security_event_daily_aggregates
-- ============================================================================
ALTER TABLE security_event_daily_aggregates
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 8: Fix security_event_daily_aggregates COALESCE unique index
-- Same approach as migration 027 for usage tables
-- ============================================================================

-- Step 1: Drop the expression-based unique index
DROP INDEX IF EXISTS idx_security_event_daily_unique;

-- Step 2: Update existing NULL values to empty strings
UPDATE security_event_daily_aggregates
SET
    user_id = COALESCE(user_id, ''),
    service_id = COALESCE(service_id, '')
WHERE
    user_id IS NULL
    OR service_id IS NULL;

-- Step 3: Add NOT NULL constraints with empty string defaults
ALTER TABLE security_event_daily_aggregates
    ALTER COLUMN user_id SET NOT NULL,
    ALTER COLUMN user_id SET DEFAULT '',
    ALTER COLUMN service_id SET NOT NULL,
    ALTER COLUMN service_id SET DEFAULT '';

-- Step 4: Add proper UNIQUE CONSTRAINT on plain columns (idempotent)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'uq_security_event_daily_dimensions'
    ) THEN
        ALTER TABLE security_event_daily_aggregates
        ADD CONSTRAINT uq_security_event_daily_dimensions
        UNIQUE (tenant_id, user_id, service_id, event_type, date);
    END IF;
END $$;
