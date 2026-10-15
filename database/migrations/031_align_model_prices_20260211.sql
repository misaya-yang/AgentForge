-- ============================================================================
-- Migration: 031_align_model_prices_20260211
-- Purpose:
--   Align default model pricing with current official provider pricing.
--   Fixes underestimated cost calculations in dashboard/usage.
-- ============================================================================

BEGIN;

-- ============================================================================
-- 1) Align default tenant llm_models prices
-- ============================================================================
UPDATE llm_models
SET input_price_per_1k = 0.0011,
    output_price_per_1k = 0.0044,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'o1-mini';

UPDATE llm_models
SET input_price_per_1k = 0.00028,
    output_price_per_1k = 0.00042,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'deepseek-chat';

UPDATE llm_models
SET input_price_per_1k = 0.00028,
    output_price_per_1k = 0.00042,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'deepseek-reasoner';

UPDATE llm_models
SET input_price_per_1k = 0.00125,
    output_price_per_1k = 0.01,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'gemini-2.5-pro';

UPDATE llm_models
SET input_price_per_1k = 0.0003,
    output_price_per_1k = 0.0025,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'gemini-2.5-flash';

UPDATE llm_models
SET input_price_per_1k = 0.0001,
    output_price_per_1k = 0.0004,
    updated_at = CURRENT_TIMESTAMP
WHERE tenant_id = 'default' AND model_id = 'gemini-2.5-flash-lite';

-- ============================================================================
-- 2) Align model_pricing prices used by UsageRecorder
-- ============================================================================
INSERT INTO model_pricing (model, provider, display_name, input_price_per_1k, output_price_per_1k, context_window, max_output_tokens, supports_vision, supports_tools)
VALUES
    ('o1-mini', 'openai', 'O1 Mini', 0.0011, 0.0044, 128000, 65536, FALSE, TRUE),
    ('deepseek-chat', 'deepseek', 'DeepSeek Chat', 0.00028, 0.00042, 64000, 8192, FALSE, TRUE),
    ('deepseek-coder', 'deepseek', 'DeepSeek Coder', 0.00028, 0.00042, 64000, 8192, FALSE, TRUE),
    ('deepseek-reasoner', 'deepseek', 'DeepSeek Reasoner (R1)', 0.00028, 0.00042, 64000, 8192, FALSE, TRUE),
    ('gemini-3-pro-preview', 'google', 'Gemini 3 Pro', 0.002, 0.012, 1000000, 65536, TRUE, TRUE),
    ('gemini-3-flash-preview', 'google', 'Gemini 3 Flash', 0.0005, 0.003, 1000000, 65536, TRUE, TRUE),
    ('gemini-2.5-pro', 'google', 'Gemini 2.5 Pro', 0.00125, 0.01, 1000000, 65536, TRUE, TRUE),
    ('gemini-2.5-flash', 'google', 'Gemini 2.5 Flash', 0.0003, 0.0025, 1000000, 65536, TRUE, TRUE),
    ('gemini-2.5-flash-lite', 'google', 'Gemini 2.5 Flash Lite', 0.0001, 0.0004, 1000000, 65536, TRUE, TRUE),
    ('gemini-2.0-flash', 'google', 'Gemini 2.0 Flash', 0.00015, 0.0006, 1000000, 8192, TRUE, TRUE),
    ('gemini-2.0-flash-thinking', 'google', 'Gemini 2.0 Flash Thinking', 0.00015, 0.0006, 1000000, 8192, TRUE, TRUE),
    ('gemini-3.0-pro', 'google', 'Gemini 3.0 Pro', 0.002, 0.012, 1000000, 8192, TRUE, TRUE),
    ('gemini-3.0-flash', 'google', 'Gemini 3.0 Flash', 0.0005, 0.003, 1000000, 8192, TRUE, TRUE)
ON CONFLICT (model) DO UPDATE SET
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    updated_at = CURRENT_TIMESTAMP;

COMMIT;
