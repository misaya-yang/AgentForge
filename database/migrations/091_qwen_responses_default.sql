-- Migration: make the system default Qwen provider use its native Responses API.
-- Tenant-created providers are untouched; their model capability overrides
-- remain the authority for an explicit compatibility fallback.

BEGIN;

UPDATE llm_providers
   SET metadata = jsonb_set(
           COALESCE(metadata, '{}'::jsonb),
           '{wire_protocol}',
           '"responses_v1"'::jsonb,
           true
       ),
       updated_at = NOW()
 WHERE tenant_id = 'default'
   AND provider_id = 'dashscope'
   AND COALESCE(metadata->>'wire_protocol', '') IS DISTINCT FROM 'responses_v1';

COMMIT;
