-- ============================================================
-- Migration: 026_memory_tenant_isolation
-- Description: Fix multi-tenant isolation for memory tables
-- Date: 2026-01-22
-- Issue: P0-1 - Memory service queries don't filter by tenant_id,
--        allowing cross-tenant data access
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. Update user_memory unique constraint to include tenant_id
-- ============================================================

-- Drop the old unique constraint (user_id, key) which doesn't include tenant_id
ALTER TABLE user_memory DROP CONSTRAINT IF EXISTS user_memory_user_id_key_key;

-- Add new composite unique constraint including tenant_id.
-- This ensures (tenant_id, user_id, key) combinations are unique.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'user_memory_tenant_user_key_unique'
    ) THEN
        ALTER TABLE user_memory ADD CONSTRAINT user_memory_tenant_user_key_unique
            UNIQUE (tenant_id, user_id, key);
    END IF;
END $$;

-- Update the ON CONFLICT target in application code to use the new constraint
COMMENT ON CONSTRAINT user_memory_tenant_user_key_unique ON user_memory IS
    'Ensures unique memory entries per tenant+user+key combination for multi-tenant isolation';

-- ============================================================
-- 2. Update session_memory unique constraint to include tenant_id
-- ============================================================

-- Drop the old unique constraint (session_id, key) which doesn't include tenant_id
ALTER TABLE session_memory DROP CONSTRAINT IF EXISTS session_memory_session_id_key_key;

-- Add new composite unique constraint including tenant_id
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'session_memory_tenant_session_key_unique'
    ) THEN
        ALTER TABLE session_memory ADD CONSTRAINT session_memory_tenant_session_key_unique
            UNIQUE (tenant_id, session_id, key);
    END IF;
END $$;

COMMENT ON CONSTRAINT session_memory_tenant_session_key_unique ON session_memory IS
    'Ensures unique memory entries per tenant+session+key combination for multi-tenant isolation';

-- ============================================================
-- 3. Create optimized indexes for tenant-filtered queries
-- ============================================================

-- Drop existing single-column indexes that will be replaced
DROP INDEX IF EXISTS idx_user_memory_user_id;
DROP INDEX IF EXISTS idx_session_memory_session_id;

-- Create composite indexes for the new query patterns
-- These indexes support WHERE tenant_id = $1 AND user_id = $2 AND key = $3
CREATE INDEX IF NOT EXISTS idx_user_memory_tenant_user_key
    ON user_memory(tenant_id, user_id, key);

CREATE INDEX IF NOT EXISTS idx_session_memory_tenant_session_key
    ON session_memory(tenant_id, session_id, key);

-- Keep the existing tenant indexes for admin/reporting queries
-- idx_user_memory_tenant_user and idx_session_memory_tenant_session remain

-- ============================================================
-- 4. Add NOT NULL constraint to tenant_id if missing
-- ============================================================

-- Ensure tenant_id cannot be NULL (defensive - should already be NOT NULL)
DO $$
BEGIN
    -- Check if user_memory.tenant_id allows NULL
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'user_memory'
        AND column_name = 'tenant_id'
        AND is_nullable = 'YES'
    ) THEN
        -- Update any NULL values to 'default' before adding constraint
        UPDATE user_memory SET tenant_id = 'default' WHERE tenant_id IS NULL;
        ALTER TABLE user_memory ALTER COLUMN tenant_id SET NOT NULL;
    END IF;

    -- Check if session_memory.tenant_id allows NULL
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'session_memory'
        AND column_name = 'tenant_id'
        AND is_nullable = 'YES'
    ) THEN
        UPDATE session_memory SET tenant_id = 'default' WHERE tenant_id IS NULL;
        ALTER TABLE session_memory ALTER COLUMN tenant_id SET NOT NULL;
    END IF;
END $$;

-- ============================================================
-- Migration complete
-- ============================================================

-- Verify constraints exist
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'user_memory_tenant_user_key_unique'
    ) THEN
        RAISE EXCEPTION 'Migration failed: user_memory_tenant_user_key_unique constraint not created';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'session_memory_tenant_session_key_unique'
    ) THEN
        RAISE EXCEPTION 'Migration failed: session_memory_tenant_session_key_unique constraint not created';
    END IF;

    RAISE NOTICE 'Migration 026_memory_tenant_isolation completed successfully';
END $$;
