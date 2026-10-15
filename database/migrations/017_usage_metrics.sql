-- Migration: 017_usage_metrics.sql
-- Description: Add usage tracking, billing, and quota management tables
-- Date: 2026-01-14

-- ============================================================================
-- 使用记录表（细粒度记录，保留30天）
-- ============================================================================
CREATE TABLE IF NOT EXISTS usage_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    request_id VARCHAR(64),

    -- 请求元数据
    service_id VARCHAR(64),
    assistant_id VARCHAR(64),
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(64),

    -- Token 使用
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER GENERATED ALWAYS AS (input_tokens + output_tokens) STORED,

    -- 成本（美分，整数精度）
    input_cost_cents INTEGER NOT NULL DEFAULT 0,
    output_cost_cents INTEGER NOT NULL DEFAULT 0,
    total_cost_cents INTEGER GENERATED ALWAYS AS (input_cost_cents + output_cost_cents) STORED,

    -- 性能指标
    latency_ms INTEGER,
    first_token_ms INTEGER,
    status VARCHAR(32) DEFAULT 'success',

    -- 请求详情
    request_type VARCHAR(32), -- chat, completion, embedding, run
    metadata JSONB DEFAULT '{}',

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 索引优化查询性能
CREATE INDEX IF NOT EXISTS idx_usage_tenant_date ON usage_records (tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_user_date ON usage_records (user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_model ON usage_records (model, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_assistant ON usage_records (assistant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_service ON usage_records (service_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_created_at ON usage_records (created_at);

-- ============================================================================
-- 每日聚合表（永久保留，加速报表查询）
-- ============================================================================
CREATE TABLE IF NOT EXISTS usage_daily_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64),
    model VARCHAR(128),
    assistant_id VARCHAR(64),
    service_id VARCHAR(64),

    date DATE NOT NULL,

    -- 聚合指标
    request_count INTEGER DEFAULT 0,
    success_count INTEGER DEFAULT 0,
    error_count INTEGER DEFAULT 0,

    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0,

    avg_latency_ms INTEGER DEFAULT 0,
    p50_latency_ms INTEGER DEFAULT 0,
    p95_latency_ms INTEGER DEFAULT 0,
    p99_latency_ms INTEGER DEFAULT 0,

    -- 首次响应时间
    avg_first_token_ms INTEGER DEFAULT 0,

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 复合唯一索引确保每日每维度只有一条聚合记录
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_daily_unique
ON usage_daily_aggregates (tenant_id, COALESCE(user_id, ''), COALESCE(model, ''), COALESCE(assistant_id, ''), COALESCE(service_id, ''), date);

CREATE INDEX IF NOT EXISTS idx_usage_daily_tenant_date ON usage_daily_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_daily_user_date ON usage_daily_aggregates (user_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_daily_model_date ON usage_daily_aggregates (model, date);

-- ============================================================================
-- 每小时聚合表（用于小时粒度图表，保留7天）
-- ============================================================================
CREATE TABLE IF NOT EXISTS usage_hourly_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64),
    model VARCHAR(128),
    assistant_id VARCHAR(64),
    service_id VARCHAR(64),

    bucket_start TIMESTAMP NOT NULL,  -- 小时起始时间
    date DATE NOT NULL,               -- 日期（便于按日清理）

    -- 聚合指标
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

-- 复合唯一索引确保每小时每维度只有一条聚合记录
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_hourly_unique
ON usage_hourly_aggregates (tenant_id, COALESCE(user_id, ''), COALESCE(model, ''), COALESCE(assistant_id, ''), COALESCE(service_id, ''), bucket_start);

CREATE INDEX IF NOT EXISTS idx_usage_hourly_tenant_date ON usage_hourly_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_hourly_bucket ON usage_hourly_aggregates (bucket_start);

-- ============================================================================
-- 用户配额表
-- ============================================================================
CREATE TABLE IF NOT EXISTS user_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,

    -- 配额限制（NULL 表示无限制）
    daily_token_limit BIGINT,              -- 每日 Token 限制
    monthly_token_limit BIGINT,            -- 每月 Token 限制
    monthly_cost_limit_cents BIGINT,       -- 每月成本限制（美分）
    requests_per_minute INTEGER,           -- RPM 限制
    requests_per_day INTEGER,              -- 每日请求数限制

    -- 当前使用量
    current_daily_tokens BIGINT DEFAULT 0,
    current_monthly_tokens BIGINT DEFAULT 0,
    current_monthly_cost_cents BIGINT DEFAULT 0,
    current_daily_requests INTEGER DEFAULT 0,

    -- 重置时间
    daily_reset_at TIMESTAMP,
    monthly_reset_at TIMESTAMP,

    -- 状态
    is_blocked BOOLEAN DEFAULT FALSE,
    blocked_reason VARCHAR(256),
    blocked_at TIMESTAMP,

    -- 告警阈值（百分比）
    warning_threshold INTEGER DEFAULT 80,   -- 使用量达到此百分比时发送警告

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,

    UNIQUE (tenant_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_quota_tenant ON user_quotas (tenant_id);
CREATE INDEX IF NOT EXISTS idx_quota_blocked ON user_quotas (is_blocked) WHERE is_blocked = TRUE;

-- ============================================================================
-- 模型定价表
-- ============================================================================
CREATE TABLE IF NOT EXISTS model_pricing (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(64),
    display_name VARCHAR(256),

    -- 定价（USD per 1K tokens）
    input_price_per_1k DECIMAL(10, 6) NOT NULL,
    output_price_per_1k DECIMAL(10, 6) NOT NULL,

    -- 模型能力
    context_window INTEGER,
    max_output_tokens INTEGER,
    supports_vision BOOLEAN DEFAULT FALSE,
    supports_tools BOOLEAN DEFAULT TRUE,
    supports_streaming BOOLEAN DEFAULT TRUE,

    -- 状态
    is_active BOOLEAN DEFAULT TRUE,
    effective_from TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    effective_to TIMESTAMP,

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,

    UNIQUE (model)
);

CREATE INDEX IF NOT EXISTS idx_pricing_provider ON model_pricing (provider);
CREATE INDEX IF NOT EXISTS idx_pricing_active ON model_pricing (is_active) WHERE is_active = TRUE;

-- ============================================================================
-- 配额告警记录表
-- ============================================================================
CREATE TABLE IF NOT EXISTS quota_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,

    alert_type VARCHAR(64) NOT NULL,  -- daily_token_warning, monthly_cost_exceeded, etc.
    threshold_value BIGINT,
    current_value BIGINT,
    limit_value BIGINT,

    message TEXT,
    is_acknowledged BOOLEAN DEFAULT FALSE,
    acknowledged_at TIMESTAMP,
    acknowledged_by VARCHAR(64),

    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_alerts_tenant_user ON quota_alerts (tenant_id, user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_alerts_unacked ON quota_alerts (is_acknowledged, created_at) WHERE is_acknowledged = FALSE;

-- ============================================================================
-- 插入默认模型定价数据
-- ============================================================================
INSERT INTO model_pricing (model, provider, display_name, input_price_per_1k, output_price_per_1k, context_window, max_output_tokens, supports_vision, supports_tools)
VALUES
    -- OpenAI Models
    ('gpt-4o', 'openai', 'GPT-4o', 0.0025, 0.01, 128000, 16384, TRUE, TRUE),
    ('gpt-4o-mini', 'openai', 'GPT-4o Mini', 0.00015, 0.0006, 128000, 16384, TRUE, TRUE),
    ('gpt-4-turbo', 'openai', 'GPT-4 Turbo', 0.01, 0.03, 128000, 4096, TRUE, TRUE),
    ('gpt-4', 'openai', 'GPT-4', 0.03, 0.06, 8192, 8192, FALSE, TRUE),
    ('gpt-3.5-turbo', 'openai', 'GPT-3.5 Turbo', 0.0005, 0.0015, 16385, 4096, FALSE, TRUE),

    -- Anthropic Models
    ('claude-3-opus-20240229', 'anthropic', 'Claude 3 Opus', 0.015, 0.075, 200000, 4096, TRUE, TRUE),
    ('claude-3-sonnet-20240229', 'anthropic', 'Claude 3 Sonnet', 0.003, 0.015, 200000, 4096, TRUE, TRUE),
    ('claude-3-5-sonnet-20241022', 'anthropic', 'Claude 3.5 Sonnet', 0.003, 0.015, 200000, 8192, TRUE, TRUE),
    ('claude-3-haiku-20240307', 'anthropic', 'Claude 3 Haiku', 0.00025, 0.00125, 200000, 4096, TRUE, TRUE),

    -- DeepSeek Models
    ('deepseek-chat', 'deepseek', 'DeepSeek Chat', 0.00014, 0.00028, 64000, 8192, FALSE, TRUE),
    ('deepseek-coder', 'deepseek', 'DeepSeek Coder', 0.00014, 0.00028, 64000, 8192, FALSE, TRUE),

    -- Alibaba DashScope Models
    ('qwen-turbo', 'dashscope', 'Qwen Turbo', 0.0008, 0.002, 8000, 2000, FALSE, TRUE),
    ('qwen-plus', 'dashscope', 'Qwen Plus', 0.004, 0.012, 32000, 2000, FALSE, TRUE),
    ('qwen-max', 'dashscope', 'Qwen Max', 0.02, 0.06, 8000, 2000, FALSE, TRUE),
    ('qwen-vl-plus', 'dashscope', 'Qwen VL Plus', 0.008, 0.008, 8000, 2000, TRUE, FALSE),

    -- LangGraph Agent (for run tracking)
    ('langgraph-agent', 'langgraph', 'LangGraph Agent', 0.001, 0.002, 128000, 8192, TRUE, TRUE),

    -- Google Gemini Models (Gemini 3.0 - 2025/2026)
    ('gemini-3.0-pro', 'google', 'Gemini 3.0 Pro', 0.002, 0.012, 1000000, 8192, TRUE, TRUE),
    ('gemini-3.0-flash', 'google', 'Gemini 3.0 Flash', 0.0005, 0.003, 1000000, 8192, TRUE, TRUE),
    -- Google Gemini Models (Gemini 2.x)
    ('gemini-2.0-flash', 'google', 'Gemini 2.0 Flash', 0.0001, 0.0004, 1000000, 8192, TRUE, TRUE),
    ('gemini-2.0-flash-thinking', 'google', 'Gemini 2.0 Flash Thinking', 0.0001, 0.0004, 1000000, 8192, TRUE, TRUE),
    ('gemini-1.5-pro', 'google', 'Gemini 1.5 Pro', 0.00125, 0.005, 2000000, 8192, TRUE, TRUE),
    ('gemini-1.5-flash', 'google', 'Gemini 1.5 Flash', 0.000075, 0.0003, 1000000, 8192, TRUE, TRUE),
    ('gemini-1.5-flash-8b', 'google', 'Gemini 1.5 Flash 8B', 0.0000375, 0.00015, 1000000, 8192, TRUE, TRUE),

    -- Default fallback
    ('default', 'unknown', 'Default Model', 0.001, 0.002, 4096, 4096, FALSE, FALSE)
ON CONFLICT (model) DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    updated_at = CURRENT_TIMESTAMP;

-- ============================================================================
-- 创建清理旧数据的函数（供定时任务调用）
-- ============================================================================
CREATE OR REPLACE FUNCTION cleanup_old_usage_records(retention_days INTEGER DEFAULT 30)
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM usage_records
    WHERE created_at < CURRENT_TIMESTAMP - (retention_days || ' days')::INTERVAL;

    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 创建更新时间戳触发器（保证迁移在非 schema 初始化环境下可运行）
-- ============================================================================
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 应用触发器到需要的表
-- ============================================================================
DROP TRIGGER IF EXISTS update_usage_daily_aggregates_updated_at ON usage_daily_aggregates;
CREATE TRIGGER update_usage_daily_aggregates_updated_at
    BEFORE UPDATE ON usage_daily_aggregates
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_user_quotas_updated_at ON user_quotas;
CREATE TRIGGER update_user_quotas_updated_at
    BEFORE UPDATE ON user_quotas
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_model_pricing_updated_at ON model_pricing;
CREATE TRIGGER update_model_pricing_updated_at
    BEFORE UPDATE ON model_pricing
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
