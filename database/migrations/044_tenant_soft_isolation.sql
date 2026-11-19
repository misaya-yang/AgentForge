-- 044_tenant_soft_isolation.sql
-- ADR-002 Phase 1: Soft Isolation — tenant tool policies, MCP configs, audit log
-- Date: 2026-04-02

SET client_encoding TO 'UTF8';

-- 1. Tenant Tool Policies — whitelist/blacklist per tenant
CREATE TABLE IF NOT EXISTS tenant_tool_policies (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            VARCHAR(64) NOT NULL UNIQUE,
    allowed_tools        TEXT[],           -- whitelist (NULL or empty = allow all)
    blocked_tools        TEXT[],           -- blacklist (takes precedence)
    allowed_categories   TEXT[],           -- e.g. {'retrieval','generation'}
    max_calls_per_session INT DEFAULT 100,
    max_calls_per_minute  INT DEFAULT 20,
    created_at           TIMESTAMPTZ DEFAULT NOW(),
    updated_at           TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ttp_tenant ON tenant_tool_policies(tenant_id);

-- 2. Tenant MCP Configs — per-tenant MCP server access
CREATE TABLE IF NOT EXISTS tenant_mcp_configs (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            VARCHAR(64) NOT NULL UNIQUE,
    allowed_servers      TEXT[] NOT NULL,
    server_overrides     JSONB,            -- e.g. {"docgen": {"url": "..."}}
    max_connections      INT DEFAULT 5,
    created_at           TIMESTAMPTZ DEFAULT NOW(),
    updated_at           TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_tmc_tenant ON tenant_mcp_configs(tenant_id);

-- 3. Tool Audit Log — all tool/skill/mcp invocations
-- TODO(P1): Partition by month + 90-day retention cron when volume exceeds ~100K rows/day
CREATE TABLE IF NOT EXISTS tool_audit_log (
    id                   BIGSERIAL PRIMARY KEY,
    tenant_id            VARCHAR(64) NOT NULL,
    user_id              VARCHAR(128) NOT NULL,
    session_id           VARCHAR(128),
    request_id           VARCHAR(128),
    tool_type            VARCHAR(16) NOT NULL,   -- tool / skill / mcp
    tool_name            VARCHAR(256) NOT NULL,
    input_summary        TEXT,
    output_status        VARCHAR(16) NOT NULL,   -- success / error / denied
    error_message        TEXT,
    latency_ms           FLOAT,
    created_at           TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_tal_tenant_user   ON tool_audit_log(tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_tal_created       ON tool_audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_tal_tool_name     ON tool_audit_log(tool_name, created_at);

-- 4. Additional indexes for existing tables
CREATE INDEX IF NOT EXISTS idx_sessions_tenant
    ON sessions(tenant_id, user_id, status);
CREATE INDEX IF NOT EXISTS idx_artifacts_tenant_user
    ON artifacts(tenant_id, user_id);
