-- Retrieval query telemetry (PRD rag-upgrade-prd-2026-08 C1 / Phase 0).
--
-- dataset_queries has existed since the original KB schema but was only ever
-- referenced by the dataset-delete cleanup — zero production writes. The
-- retrieve() pipeline appends one row per query (content + created_by) and
-- needs a structured home for query fingerprint, mode, top_k, hit_count and
-- stage timings so zero-result analysis and weight tuning have data to read.
--
-- Additive only: one nullable-in-practice JSONB column with a safe default.
-- Existing rows keep '{}'. Safe to apply repeatedly (IF NOT EXISTS).

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

ALTER TABLE dataset_queries
    ADD COLUMN IF NOT EXISTS metadata JSONB NOT NULL DEFAULT '{}';

COMMIT;
