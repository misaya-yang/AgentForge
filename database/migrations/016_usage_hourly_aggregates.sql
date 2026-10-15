-- Migration: 016_usage_hourly_aggregates.sql
-- Description: Add hourly aggregates for usage metrics
-- Date: 2026-01-15

CREATE TABLE IF NOT EXISTS usage_hourly_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64),
    model VARCHAR(128),
    assistant_id VARCHAR(64),
    service_id VARCHAR(64),

    bucket_start TIMESTAMP NOT NULL,
    date DATE NOT NULL,

    request_count INTEGER DEFAULT 0,
    success_count INTEGER DEFAULT 0,
    error_count INTEGER DEFAULT 0,

    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0,

    avg_latency_ms INTEGER DEFAULT 0,
    avg_first_token_ms INTEGER DEFAULT 0,

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_hourly_unique
ON usage_hourly_aggregates (
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(model, ''),
    COALESCE(assistant_id, ''),
    COALESCE(service_id, ''),
    bucket_start
);

CREATE INDEX IF NOT EXISTS idx_usage_hourly_tenant_date ON usage_hourly_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_hourly_bucket ON usage_hourly_aggregates (bucket_start);
