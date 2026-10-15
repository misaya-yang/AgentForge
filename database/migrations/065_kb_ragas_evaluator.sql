-- Migration: 065_kb_ragas_evaluator.sql
-- Goal: allow knowledge-base RAGAS evaluator type.

BEGIN;

ALTER TABLE eval_evaluators
    DROP CONSTRAINT IF EXISTS chk_eval_evaluators_type;

ALTER TABLE eval_evaluators
    ADD CONSTRAINT chk_eval_evaluators_type
    CHECK (evaluator_type IN ('human', 'rule', 'trajectory', 'span', 'llm', 'llm_judge', 'composite', 'ragas'));

COMMIT;