-- 055_llm_models_provider_pk.sql
--
-- Add provider_id to the llm_models primary key so the same model_id
-- (e.g. gemini-3.1-pro-preview) can coexist under multiple providers
-- — specifically ``google`` (AI Studio) and ``google-vertex`` (Vertex
-- Express Mode), which hit the same underlying model via different hosts
-- with different auth formats.
--
-- Before: PRIMARY KEY (tenant_id, model_id)
-- After:  PRIMARY KEY (tenant_id, provider_id, model_id)
--
-- Every existing row already has a non-null provider_id, so this is a
-- pure constraint change with zero row rewrites.

BEGIN;

ALTER TABLE llm_models DROP CONSTRAINT llm_models_pkey;

ALTER TABLE llm_models
    ADD PRIMARY KEY (tenant_id, provider_id, model_id);

COMMIT;
