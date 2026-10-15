-- Migration: 047_add_qwen36_plus.sql
-- Description: Add Qwen 3.6 Plus model to LLM models catalog
-- Date: 2026-04-07

INSERT INTO llm_models (model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, input_price_per_1k, output_price_per_1k, access_level, is_enabled, sort_order)
VALUES ('qwen3.6-plus', 'default', 'dashscope', 'Qwen 3.6 Plus', 1000000, 65536, false, true, 0.000000, 0.000000, 'public', true, 5)
ON CONFLICT (tenant_id, model_id) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    context_window = EXCLUDED.context_window,
    max_output_tokens = EXCLUDED.max_output_tokens,
    updated_at = NOW();
