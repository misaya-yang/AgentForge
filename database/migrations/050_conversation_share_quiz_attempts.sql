-- 050_conversation_share_quiz_attempts.sql
-- Anonymous quiz attempts taken from public share pages.
-- One row per (share_code, anon_id, quiz_id) — repeat submissions replay
-- the cached result rather than re-grading.
-- Date: 2026-04-21

SET client_encoding TO 'UTF8';

CREATE TABLE IF NOT EXISTS conversation_share_quiz_attempts (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    share_code    VARCHAR(12) NOT NULL,
    anon_id       VARCHAR(128) NOT NULL,
    quiz_id       UUID NOT NULL,
    -- Full result payload (per_question array, scores, etc.) — mirrors the
    -- shape returned by /assistant/quiz/{id}/submit so the front-end can
    -- rehydrate a prior attempt without extra shape-shifting.
    result        JSONB NOT NULL,
    submitted_at  TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (share_code, anon_id, quiz_id)
);

CREATE INDEX IF NOT EXISTS idx_csqa_share_code
    ON conversation_share_quiz_attempts(share_code);
CREATE INDEX IF NOT EXISTS idx_csqa_quiz_id
    ON conversation_share_quiz_attempts(quiz_id);
