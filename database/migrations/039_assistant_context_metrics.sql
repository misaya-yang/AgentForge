-- Migration: 039_assistant_context_metrics.sql
-- Goal: Add fine-grained context breakdown metrics table

BEGIN;

CREATE TABLE IF NOT EXISTS assistant_context_breakdown (
    breakdown_id UUID PRIMARY KEY,
    request_id VARCHAR(64) NOT NULL,
    run_id VARCHAR(64),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100),
    model_id VARCHAR(128),
    total_tokens INTEGER NOT NULL DEFAULT 0,
    total_chars INTEGER NOT NULL DEFAULT 0,
    tokens_by_category JSONB NOT NULL DEFAULT '{}'::jsonb,
    top_contributors JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_context_breakdown_tenant_created
    ON assistant_context_breakdown(tenant_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_assistant_context_breakdown_request
    ON assistant_context_breakdown(request_id);

COMMIT;
