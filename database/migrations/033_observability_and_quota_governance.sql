-- Migration: 033_observability_and_quota_governance.sql
-- Goal:
-- 1) Add fine-grained latency/error attribution fields for usage records
-- 2) Persist sampled request traces for dashboard trace panel
-- 3) Add quota governance strategy fields
-- 4) Add API key model allowlist for policy enforcement

BEGIN;

-- -------------------------------------------------------------------------
-- usage_records: observability fields
-- -------------------------------------------------------------------------
ALTER TABLE usage_records
    ADD COLUMN IF NOT EXISTS request_total_duration_ms INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS llm_inference_duration_ms INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS retrieval_duration_ms INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS tool_call_duration_ms INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS agent_or_graph_overhead_ms INTEGER DEFAULT 0,
    ADD COLUMN IF NOT EXISTS tool_call_breakdown JSONB DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS error_type VARCHAR(32),
    ADD COLUMN IF NOT EXISTS trace_sampled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS sample_reason VARCHAR(32);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_usage_records_error_type'
    ) THEN
        ALTER TABLE usage_records
        ADD CONSTRAINT chk_usage_records_error_type
        CHECK (
            error_type IS NULL OR
            error_type IN (
                'timeout',
                'provider_error',
                'rate_limit',
                'auth_error',
                'tool_error',
                'content_filter',
                'unknown'
            )
        );
    END IF;
END $$;

-- -------------------------------------------------------------------------
-- request_traces: sampled trace persistence for dashboard querying
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS request_traces (
    trace_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id VARCHAR(64) NOT NULL,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    service_id VARCHAR(64) NOT NULL,
    assistant_id VARCHAR(64) NOT NULL DEFAULT '',
    provider VARCHAR(64) NOT NULL,
    model VARCHAR(128) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'success',
    error_type VARCHAR(32),
    request_total_duration_ms INTEGER NOT NULL DEFAULT 0,
    first_token_latency_ms INTEGER NOT NULL DEFAULT 0,
    llm_inference_duration_ms INTEGER NOT NULL DEFAULT 0,
    retrieval_duration_ms INTEGER NOT NULL DEFAULT 0,
    tool_call_duration_ms INTEGER NOT NULL DEFAULT 0,
    agent_or_graph_overhead_ms INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_cost_cents INTEGER NOT NULL DEFAULT 0,
    sample_reason VARCHAR(32) NOT NULL DEFAULT 'baseline',
    trace_steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_request_traces_tenant_created
    ON request_traces(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_request_id
    ON request_traces(request_id);
CREATE INDEX IF NOT EXISTS idx_request_traces_service
    ON request_traces(service_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_user
    ON request_traces(user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_provider_model
    ON request_traces(provider, model, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_status_error
    ON request_traces(status, error_type, created_at DESC);

-- -------------------------------------------------------------------------
-- user_quotas: governance strategy fields
-- -------------------------------------------------------------------------
ALTER TABLE user_quotas
    ADD COLUMN IF NOT EXISTS overage_strategy VARCHAR(32) NOT NULL DEFAULT 'allow_but_alert',
    ADD COLUMN IF NOT EXISTS downgraded_model VARCHAR(128),
    ADD COLUMN IF NOT EXISTS temporary_extra_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS temporary_extra_cost_cents BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS temporary_expires_at TIMESTAMPTZ;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_user_quotas_overage_strategy'
    ) THEN
        ALTER TABLE user_quotas
        ADD CONSTRAINT chk_user_quotas_overage_strategy
        CHECK (overage_strategy IN ('hard_block', 'rate_limit', 'downgrade_model', 'allow_but_alert'));
    END IF;
END $$;

-- -------------------------------------------------------------------------
-- api_keys: model allowlist for key-level governance
-- -------------------------------------------------------------------------
ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS allowed_models VARCHAR(255)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(255)[];

CREATE INDEX IF NOT EXISTS idx_api_keys_allowed_models_gin
    ON api_keys USING GIN (allowed_models);

COMMIT;
