-- Migration: 024_assistant_memory.sql
-- Description: Create three-layer memory system tables for session and user memory
-- Date: 2026-01-16

-- ============================================================================
-- Session Memory Table
-- Stores session-specific key-value pairs that persist across reconnections
-- Cleared when session is deleted
-- ============================================================================

CREATE TABLE IF NOT EXISTS session_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(100) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    key VARCHAR(255) NOT NULL,
    value JSONB NOT NULL,
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (session_id, key)
);

-- Indexes for efficient querying
CREATE INDEX IF NOT EXISTS idx_session_memory_session_id
    ON session_memory(session_id);

CREATE INDEX IF NOT EXISTS idx_session_memory_tenant_session
    ON session_memory(tenant_id, session_id);

-- ============================================================================
-- User Memory Table (Long-term Memory)
-- Stores user-level persistent memory for preferences, patterns, and learned info
-- Persists across sessions and is tied to the user account
-- ============================================================================

CREATE TABLE IF NOT EXISTS user_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(100) NOT NULL,
    user_id VARCHAR(100) NOT NULL,
    key VARCHAR(255) NOT NULL,
    value JSONB NOT NULL,
    metadata JSONB,
    access_count INTEGER DEFAULT 0,
    last_accessed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (user_id, key)
);

-- Indexes for efficient querying
CREATE INDEX IF NOT EXISTS idx_user_memory_user_id
    ON user_memory(user_id);

CREATE INDEX IF NOT EXISTS idx_user_memory_tenant_user
    ON user_memory(tenant_id, user_id);

-- Index on access_count for finding frequently used items (hot memory)
CREATE INDEX IF NOT EXISTS idx_user_memory_access_count
    ON user_memory(access_count DESC)
    WHERE access_count > 0;

-- ============================================================================
-- Trigger for automatic updated_at timestamp update
-- ============================================================================
-- Use independent function name to avoid conflicts with other migrations
CREATE OR REPLACE FUNCTION update_memory_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_session_memory_timestamp ON session_memory;
CREATE TRIGGER update_session_memory_timestamp
    BEFORE UPDATE ON session_memory
    FOR EACH ROW
    EXECUTE FUNCTION update_memory_timestamp();

DROP TRIGGER IF EXISTS update_user_memory_timestamp ON user_memory;
CREATE TRIGGER update_user_memory_timestamp
    BEFORE UPDATE ON user_memory
    FOR EACH ROW
    EXECUTE FUNCTION update_memory_timestamp();

-- ============================================================================
-- Comments and Documentation
-- ============================================================================

COMMENT ON TABLE session_memory IS 'Session-specific memory that persists across reconnections until session deletion';
COMMENT ON COLUMN session_memory.id IS 'Unique identifier for memory entry';
COMMENT ON COLUMN session_memory.tenant_id IS 'Tenant ID for multi-tenancy isolation';
COMMENT ON COLUMN session_memory.session_id IS 'Associated session ID';
COMMENT ON COLUMN session_memory.key IS 'Memory key for key-value lookup';
COMMENT ON COLUMN session_memory.value IS 'JSONB value stored in memory';
COMMENT ON COLUMN session_memory.metadata IS 'Additional metadata about the memory entry';

COMMENT ON TABLE user_memory IS 'User-level long-term memory for preferences, patterns, and learned information';
COMMENT ON COLUMN user_memory.id IS 'Unique identifier for memory entry';
COMMENT ON COLUMN user_memory.tenant_id IS 'Tenant ID for multi-tenancy isolation';
COMMENT ON COLUMN user_memory.user_id IS 'Associated user ID';
COMMENT ON COLUMN user_memory.key IS 'Memory key for key-value lookup';
COMMENT ON COLUMN user_memory.value IS 'JSONB value stored in memory';
COMMENT ON COLUMN user_memory.metadata IS 'Additional metadata about the memory entry';
COMMENT ON COLUMN user_memory.access_count IS 'Number of times this memory entry has been accessed (for finding hot memory)';
COMMENT ON COLUMN user_memory.last_accessed_at IS 'Timestamp of last access to this memory entry';
