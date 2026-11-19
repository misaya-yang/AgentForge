-- 041_quiz_tables.sql
-- Quiz system tables for AI Assistant quiz feature
-- Date: 2026-04-01

SET client_encoding TO 'UTF8';

-- Quiz definition
CREATE TABLE IF NOT EXISTS quizzes (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     VARCHAR(64) NOT NULL,
    created_by    VARCHAR(128) NOT NULL,
    title         VARCHAR(500) NOT NULL,
    description   TEXT,
    dataset_ids   JSONB NOT NULL DEFAULT '[]'::jsonb,
    topic         VARCHAR(500),
    question_count INT NOT NULL DEFAULT 5,
    difficulty    VARCHAR(20) NOT NULL DEFAULT 'medium',
    config        JSONB NOT NULL DEFAULT '{}'::jsonb,
    status        VARCHAR(20) NOT NULL DEFAULT 'ready',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_quizzes_tenant ON quizzes(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_quizzes_created_by ON quizzes(created_by, created_at DESC);

-- Individual questions
CREATE TABLE IF NOT EXISTS quiz_questions (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quiz_id       UUID NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    question_num  INT NOT NULL,
    question_type VARCHAR(20) NOT NULL DEFAULT 'mc_single',
    question_text TEXT NOT NULL,
    options       JSONB NOT NULL DEFAULT '[]'::jsonb,
    correct_answer JSONB NOT NULL,
    explanation   TEXT,
    source_chunks JSONB DEFAULT '[]'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(quiz_id, question_num)
);

CREATE INDEX IF NOT EXISTS idx_quiz_questions_quiz ON quiz_questions(quiz_id, question_num);

-- Quiz attempt (each time someone takes the quiz)
CREATE TABLE IF NOT EXISTS quiz_attempts (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quiz_id       UUID NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    user_id       VARCHAR(128),
    share_id      UUID,
    display_name  VARCHAR(200),
    answers       JSONB NOT NULL DEFAULT '[]'::jsonb,
    total_score   NUMERIC(5,2),
    correct_count INT DEFAULT 0,
    total_count   INT DEFAULT 0,
    started_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at  TIMESTAMPTZ,
    status        VARCHAR(20) NOT NULL DEFAULT 'in_progress'
);

CREATE INDEX IF NOT EXISTS idx_quiz_attempts_quiz ON quiz_attempts(quiz_id);
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_user ON quiz_attempts(user_id, started_at DESC);

-- Shareable link (Phase 2 — table created now for schema completeness)
CREATE TABLE IF NOT EXISTS quiz_shares (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quiz_id       UUID NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    share_code    VARCHAR(20) NOT NULL UNIQUE,
    created_by    VARCHAR(128) NOT NULL,
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    max_attempts  INT,
    expires_at    TIMESTAMPTZ,
    require_name  BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_quiz_shares_code ON quiz_shares(share_code);
