-- Migration: 064_eval_platform_contracts.sql
-- Goal: additive Eval Platform contract hardening.

BEGIN;

ALTER TABLE eval_evaluators
    DROP CONSTRAINT IF EXISTS chk_eval_evaluators_type;

ALTER TABLE eval_evaluators
    ADD CONSTRAINT chk_eval_evaluators_type
    CHECK (evaluator_type IN ('human', 'rule', 'trajectory', 'span', 'llm', 'llm_judge', 'composite'));

COMMIT;
