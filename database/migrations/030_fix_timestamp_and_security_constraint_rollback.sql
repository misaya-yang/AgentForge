-- Rollback for Migration 030: fix_timestamp_and_security_constraint
-- Description: Rollback TIMESTAMPTZ → TIMESTAMP conversions and security_event constraints
-- Date: 2026-01-30
--
-- WARNING: This rollback also requires table rewrites and will block reads/writes.
-- Only run during a maintenance window if rolling back is absolutely necessary.
--
-- Note: This rollback script assumes the migration 030 was fully completed.
-- Partial rollback may require manual intervention.

-- ============================================================================
-- Part 1: Rollback usage_records TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE usage_records
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 2: Rollback usage_daily_aggregates TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE usage_daily_aggregates
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 3: Rollback usage_hourly_aggregates TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE usage_hourly_aggregates
    ALTER COLUMN bucket_start TYPE TIMESTAMP USING bucket_start AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 4: Rollback user_quotas TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE user_quotas
    ALTER COLUMN daily_reset_at TYPE TIMESTAMP USING daily_reset_at AT TIME ZONE 'UTC',
    ALTER COLUMN monthly_reset_at TYPE TIMESTAMP USING monthly_reset_at AT TIME ZONE 'UTC',
    ALTER COLUMN blocked_at TYPE TIMESTAMP USING blocked_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 5: Rollback model_pricing TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE model_pricing
    ALTER COLUMN effective_from TYPE TIMESTAMP USING effective_from AT TIME ZONE 'UTC',
    ALTER COLUMN effective_to TYPE TIMESTAMP USING effective_to AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 6: Rollback quota_alerts TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE quota_alerts
    ALTER COLUMN acknowledged_at TYPE TIMESTAMP USING acknowledged_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 7: Rollback security_event_daily_aggregates TIMESTAMPTZ → TIMESTAMP
-- ============================================================================
ALTER TABLE security_event_daily_aggregates
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';

-- ============================================================================
-- Part 8: Rollback security_event_daily_aggregates constraints
-- ============================================================================

-- Step 1: Drop the named unique constraint
ALTER TABLE security_event_daily_aggregates
    DROP CONSTRAINT IF EXISTS uq_security_event_daily_dimensions;

-- Step 2: Restore NULL-friendly defaults (allow NULL values again)
ALTER TABLE security_event_daily_aggregates
    ALTER COLUMN user_id DROP NOT NULL,
    ALTER COLUMN user_id DROP DEFAULT,
    ALTER COLUMN service_id DROP NOT NULL,
    ALTER COLUMN service_id DROP DEFAULT;

-- Step 3: Convert empty strings back to NULL (reverse of migration 030)
UPDATE security_event_daily_aggregates
SET
    user_id = NULL
WHERE user_id = '';

UPDATE security_event_daily_aggregates
SET
    service_id = NULL
WHERE service_id = '';

-- Step 4: Recreate the original expression-based unique index
CREATE UNIQUE INDEX IF NOT EXISTS idx_security_event_daily_unique
ON security_event_daily_aggregates (
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(service_id, ''),
    event_type,
    date
);

-- ============================================================================
-- Rollback Complete
-- ============================================================================
-- Note: After rollback, the database will be back to using TIMESTAMP (no TZ)
-- and expression-based unique index on security_event_daily_aggregates.
-- Verify application behavior after rollback.
