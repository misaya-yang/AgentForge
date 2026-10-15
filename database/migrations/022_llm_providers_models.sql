-- Migration: 022_llm_providers_models.sql
-- Description: Add tables for dynamic LLM provider and model management
-- Date: 2026-01-16

-- ============================================================================
-- LLM Providers Table
-- Stores API provider configurations (OpenAI, Anthropic, custom providers, etc.)
-- ============================================================================

CREATE TABLE IF NOT EXISTS llm_providers (
    provider_id VARCHAR(50) NOT NULL,
    tenant_id VARCHAR(100) NOT NULL,
    display_name VARCHAR(100) NOT NULL,
    api_type VARCHAR(20) DEFAULT 'openai',  -- 'openai' | 'anthropic' | 'google'
    base_url VARCHAR(500),                   -- Custom API base URL
    api_key_encrypted TEXT,                  -- Encrypted API key
    is_enabled BOOLEAN DEFAULT true,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (tenant_id, provider_id)
);

COMMENT ON TABLE llm_providers IS 'LLM API provider configurations';
COMMENT ON COLUMN llm_providers.provider_id IS 'Unique provider identifier (e.g., openai, anthropic, moonshot)';
COMMENT ON COLUMN llm_providers.api_type IS 'API compatibility type: openai, anthropic, or google';
COMMENT ON COLUMN llm_providers.base_url IS 'Custom API endpoint URL (null uses default)';
COMMENT ON COLUMN llm_providers.api_key_encrypted IS 'Fernet-encrypted API key';

-- ============================================================================
-- LLM Models Table
-- Stores model configurations linked to providers
-- ============================================================================

CREATE TABLE IF NOT EXISTS llm_models (
    model_id VARCHAR(100) NOT NULL,
    tenant_id VARCHAR(100) NOT NULL,
    provider_id VARCHAR(50) NOT NULL,
    display_name VARCHAR(100) NOT NULL,
    context_window INT DEFAULT 128000,
    max_output_tokens INT DEFAULT 4096,
    supports_vision BOOLEAN DEFAULT false,
    supports_tools BOOLEAN DEFAULT true,
    input_price_per_1k DECIMAL(10,6) DEFAULT 0,   -- USD per 1K input tokens
    output_price_per_1k DECIMAL(10,6) DEFAULT 0,  -- USD per 1K output tokens
    access_level VARCHAR(20) DEFAULT 'public',    -- 'public' | 'premium' | 'admin'
    is_enabled BOOLEAN DEFAULT true,
    sort_order INT DEFAULT 0,                      -- For custom ordering in UI
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    PRIMARY KEY (tenant_id, model_id)
);

COMMENT ON TABLE llm_models IS 'LLM model configurations';
COMMENT ON COLUMN llm_models.model_id IS 'Model identifier sent to API (e.g., gpt-4o, claude-3-5-sonnet)';
COMMENT ON COLUMN llm_models.access_level IS 'User access level required: public, premium, or admin';
COMMENT ON COLUMN llm_models.sort_order IS 'Custom sort order for UI display';

-- ============================================================================
-- Indexes
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_llm_providers_tenant
    ON llm_providers(tenant_id);

CREATE INDEX IF NOT EXISTS idx_llm_providers_enabled
    ON llm_providers(tenant_id, is_enabled);

CREATE INDEX IF NOT EXISTS idx_llm_models_tenant
    ON llm_models(tenant_id);

CREATE INDEX IF NOT EXISTS idx_llm_models_provider
    ON llm_models(tenant_id, provider_id);

CREATE INDEX IF NOT EXISTS idx_llm_models_enabled
    ON llm_models(tenant_id, is_enabled);

-- ============================================================================
-- Seed Default Providers (for default tenant)
-- These are the built-in providers that come with the system
-- ============================================================================

INSERT INTO llm_providers (provider_id, tenant_id, display_name, api_type, base_url, is_enabled)
VALUES
    ('openai', 'default', 'OpenAI', 'openai', 'https://api.openai.com', true),
    ('anthropic', 'default', 'Anthropic', 'anthropic', 'https://api.anthropic.com', true),
    ('deepseek', 'default', 'DeepSeek', 'openai', 'https://api.deepseek.com', true),
    ('dashscope', 'default', 'Qwen/DashScope', 'openai', 'https://dashscope.aliyuncs.com/compatible-mode', true),
    ('google', 'default', 'Google Gemini', 'google', 'https://generativelanguage.googleapis.com', true)
ON CONFLICT (tenant_id, provider_id) DO NOTHING;

-- ============================================================================
-- Seed Default Models (matching existing DEFAULT_MODELS in model_registry.py)
-- ============================================================================

-- OpenAI models
INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, sort_order)
VALUES
    ('gpt-4o', 'default', 'openai', 'GPT-4o', 128000, 16384, true, true, 0.0025, 0.01, 'public', 1),
    ('gpt-4o-mini', 'default', 'openai', 'GPT-4o Mini', 128000, 16384, true, true, 0.00015, 0.0006, 'public', 2),
    ('o1', 'default', 'openai', 'O1', 200000, 100000, true, true, 0.015, 0.06, 'admin', 3),
    ('o1-mini', 'default', 'openai', 'O1 Mini', 128000, 65536, false, true, 0.003, 0.012, 'premium', 4)
ON CONFLICT (tenant_id, model_id) 
DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_vision = EXCLUDED.supports_vision;

-- Anthropic models
INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, sort_order)
VALUES
    ('claude-sonnet-4-20250514', 'default', 'anthropic', 'Claude Sonnet 4', 200000, 64000, true, true, 0.003, 0.015, 'public', 1),
    ('claude-3-5-sonnet-20241022', 'default', 'anthropic', 'Claude 3.5 Sonnet', 200000, 8192, true, true, 0.003, 0.015, 'public', 2),
    ('claude-3-5-haiku-20241022', 'default', 'anthropic', 'Claude 3.5 Haiku', 200000, 8192, true, true, 0.0008, 0.004, 'public', 3)
ON CONFLICT (tenant_id, model_id) 
DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_vision = EXCLUDED.supports_vision;

-- DeepSeek models
INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, sort_order)
VALUES
    ('deepseek-chat', 'default', 'deepseek', 'DeepSeek Chat', 64000, 8192, false, true, 0.00014, 0.00028, 'public', 1),
    ('deepseek-reasoner', 'default', 'deepseek', 'DeepSeek Reasoner (R1)', 64000, 8192, false, true, 0.00055, 0.00219, 'public', 2)
ON CONFLICT (tenant_id, model_id) 
DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_vision = EXCLUDED.supports_vision;

-- DashScope/Qwen models
INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, sort_order)
VALUES
    ('qwen-turbo', 'default', 'dashscope', 'Qwen Turbo', 131072, 8192, false, true, 0.0003, 0.0006, 'public', 1),
    ('qwen-plus', 'default', 'dashscope', 'Qwen Plus', 131072, 8192, false, true, 0.0004, 0.0012, 'public', 2),
    ('qwen-max', 'default', 'dashscope', 'Qwen Max', 32768, 8192, false, true, 0.0012, 0.006, 'premium', 3),
    ('qwen-vl-max', 'default', 'dashscope', 'Qwen VL Max', 32768, 8192, true, true, 0.00023, 0.000574, 'premium', 4)
ON CONFLICT (tenant_id, model_id) 
DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_vision = EXCLUDED.supports_vision;

-- Google Gemini models
INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, sort_order)
VALUES
    ('gemini-3-pro-preview', 'default', 'google', 'Gemini 3 Pro', 1000000, 65536, true, true, 0.002, 0.012, 'admin', 1),
    ('gemini-3-flash-preview', 'default', 'google', 'Gemini 3 Flash', 1000000, 65536, true, true, 0.0005, 0.003, 'admin', 2),
    ('gemini-2.5-pro', 'default', 'google', 'Gemini 2.5 Pro', 1000000, 65536, true, true, 0.00125, 0.005, 'admin', 3),
    ('gemini-2.5-flash', 'default', 'google', 'Gemini 2.5 Flash', 1000000, 65536, true, true, 0.000075, 0.0003, 'premium', 4),
    ('gemini-2.5-flash-lite', 'default', 'google', 'Gemini 2.5 Flash Lite', 1000000, 65536, true, true, 0.00005, 0.0002, 'public', 5)
ON CONFLICT (tenant_id, model_id) 
DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_vision = EXCLUDED.supports_vision;
