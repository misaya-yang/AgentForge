-- Migration: 069_add_qwen37_plus_default.sql
-- Description: Add Qwen 3.7 Plus and make it the preferred DashScope default.
-- Date: 2026-07-01

INSERT INTO llm_models (
    model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens,
    supports_vision, supports_tools, input_price_per_1k, output_price_per_1k,
    access_level, is_enabled, sort_order
)
VALUES (
    'qwen3.7-plus', 'default', 'dashscope', 'Qwen 3.7 Plus', 1000000, 65536,
    false, true, 0.000500, 0.003000, 'public', true, 1
)
ON CONFLICT (tenant_id, provider_id, model_id) DO UPDATE SET
    provider_id = EXCLUDED.provider_id,
    display_name = EXCLUDED.display_name,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    supports_tools = EXCLUDED.supports_tools,
    input_price_per_1k = EXCLUDED.input_price_per_1k,
    output_price_per_1k = EXCLUDED.output_price_per_1k,
    access_level = EXCLUDED.access_level,
    is_enabled = EXCLUDED.is_enabled,
    sort_order = EXCLUDED.sort_order,
    updated_at = NOW();

UPDATE llm_models
SET sort_order = GREATEST(sort_order, 5), updated_at = NOW()
WHERE tenant_id = 'default'
  AND provider_id = 'dashscope'
  AND model_id <> 'qwen3.7-plus'
  AND sort_order < 5;
