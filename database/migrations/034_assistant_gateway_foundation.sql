-- Migration: 034_assistant_gateway_foundation.sql
-- Goal:
-- 1) Add Assistant Gateway runtime tables (runs, command queue, approvals)
-- 2) Extend assistant memory tables with namespace/ttl/sensitivity/source fields

BEGIN;

-- -------------------------------------------------------------------------
-- Memory v2 metadata columns
-- -------------------------------------------------------------------------
ALTER TABLE session_memory
    ADD COLUMN IF NOT EXISTS namespace VARCHAR(64) NOT NULL DEFAULT 'default',
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS sensitivity VARCHAR(32) NOT NULL DEFAULT 'normal',
    ADD COLUMN IF NOT EXISTS source VARCHAR(64) NOT NULL DEFAULT 'assistant';

ALTER TABLE user_memory
    ADD COLUMN IF NOT EXISTS namespace VARCHAR(64) NOT NULL DEFAULT 'default',
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS sensitivity VARCHAR(32) NOT NULL DEFAULT 'normal',
    ADD COLUMN IF NOT EXISTS source VARCHAR(64) NOT NULL DEFAULT 'assistant';

-- Best-effort namespace backfill from key prefix: namespace/key
UPDATE session_memory
SET namespace = split_part(key, '/', 1)
WHERE position('/' IN key) > 0
  AND (namespace IS NULL OR namespace = 'default');

UPDATE user_memory
SET namespace = split_part(key, '/', 1)
WHERE position('/' IN key) > 0
  AND (namespace IS NULL OR namespace = 'default');

CREATE INDEX IF NOT EXISTS idx_session_memory_namespace_expires
    ON session_memory(tenant_id, session_id, namespace, expires_at);

CREATE INDEX IF NOT EXISTS idx_user_memory_namespace_expires
    ON user_memory(tenant_id, user_id, namespace, expires_at);

-- -------------------------------------------------------------------------
-- Assistant run tracking
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant_runs (
    run_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    engine VARCHAR(32) NOT NULL DEFAULT 'agent_loop',
    execution_profile VARCHAR(16) NOT NULL DEFAULT 'safe',
    memory_mode VARCHAR(16) NOT NULL DEFAULT 'auto',
    os_agent_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    request_preview TEXT NOT NULL DEFAULT '',
    usage JSONB NOT NULL DEFAULT '{}'::jsonb,
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_runs_tenant_user_created
    ON assistant_runs(tenant_id, user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_assistant_runs_session_created
    ON assistant_runs(session_id, created_at DESC);

-- -------------------------------------------------------------------------
-- Assistant command queue
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant_command_queue (
    command_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    run_id UUID,
    command_key UUID NOT NULL,
    tool_name VARCHAR(100) NOT NULL,
    arguments JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    retry_count INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 2,
    lease_expires_at TIMESTAMPTZ,
    result JSONB,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_tenant_status
    ON assistant_command_queue(tenant_id, status, created_at);

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_session_status
    ON assistant_command_queue(session_id, status, created_at);

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_command_key
    ON assistant_command_queue(command_key, status);

-- -------------------------------------------------------------------------
-- Assistant tool approvals
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant_tool_approvals (
    approval_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    run_id UUID,
    tool_name VARCHAR(100) NOT NULL,
    arguments JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    reason TEXT,
    approved_by VARCHAR(64),
    approved_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_tool_approvals_tenant_user
    ON assistant_tool_approvals(tenant_id, user_id, status, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_assistant_tool_approvals_session
    ON assistant_tool_approvals(session_id, status, created_at DESC);

-- -------------------------------------------------------------------------
-- Update timestamp trigger for new assistant tables
-- -------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION update_assistant_gateway_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_assistant_runs_timestamp ON assistant_runs;
CREATE TRIGGER update_assistant_runs_timestamp
    BEFORE UPDATE ON assistant_runs
    FOR EACH ROW
    EXECUTE FUNCTION update_assistant_gateway_timestamp();

DROP TRIGGER IF EXISTS update_assistant_queue_timestamp ON assistant_command_queue;
CREATE TRIGGER update_assistant_queue_timestamp
    BEFORE UPDATE ON assistant_command_queue
    FOR EACH ROW
    EXECUTE FUNCTION update_assistant_gateway_timestamp();

DROP TRIGGER IF EXISTS update_assistant_approvals_timestamp ON assistant_tool_approvals;
CREATE TRIGGER update_assistant_approvals_timestamp
    BEFORE UPDATE ON assistant_tool_approvals
    FOR EACH ROW
    EXECUTE FUNCTION update_assistant_gateway_timestamp();

COMMIT;
