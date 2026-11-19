-- 043_exam_management.sql
-- Exam management layer on top of quiz system
-- Date: 2026-04-01

SET client_encoding TO 'UTF8';

-- Exams table: a promoted quiz with admin-managed settings
CREATE TABLE IF NOT EXISTS exams (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id           VARCHAR(64) NOT NULL,
    quiz_id             UUID NOT NULL REFERENCES quizzes(id),
    title               VARCHAR(500) NOT NULL,
    description         TEXT,
    published_by        VARCHAR(128) NOT NULL,
    status              VARCHAR(20) NOT NULL DEFAULT 'draft',
    assigned_groups     JSONB NOT NULL DEFAULT '[]'::jsonb,
    deadline            TIMESTAMPTZ,
    max_retakes         INT DEFAULT 1,
    time_limit_minutes  INT,
    passing_score       NUMERIC(5,2) DEFAULT 0.60,
    settings            JSONB NOT NULL DEFAULT '{}'::jsonb,
    share_id            UUID REFERENCES quiz_shares(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_exams_tenant ON exams(tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_exams_quiz ON exams(quiz_id);

-- AI analysis reports
CREATE TABLE IF NOT EXISTS exam_analysis_reports (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id         UUID NOT NULL REFERENCES exams(id) ON DELETE CASCADE,
    report_data     JSONB NOT NULL,
    model_id        VARCHAR(128),
    generated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_exam_reports_exam ON exam_analysis_reports(exam_id);

-- Extend existing tables
ALTER TABLE quiz_shares ADD COLUMN IF NOT EXISTS is_exam BOOLEAN DEFAULT FALSE;
ALTER TABLE quiz_attempts ADD COLUMN IF NOT EXISTS exam_id UUID;
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_exam ON quiz_attempts(exam_id);
