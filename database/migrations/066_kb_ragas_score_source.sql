-- Migration: 066_kb_ragas_score_source.sql
-- Goal: allow KB RAGAS scores to persist with score_source = 'kb_ragas'.

BEGIN;

ALTER TABLE agent_trace_scores
    DROP CONSTRAINT IF EXISTS chk_agent_trace_scores_source;

ALTER TABLE agent_trace_scores
    ADD CONSTRAINT chk_agent_trace_scores_source
    CHECK (score_source IN ('human', 'llm', 'rule', 'system', 'imported', 'kb_ragas'));

COMMIT;