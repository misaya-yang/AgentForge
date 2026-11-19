-- =============================================================================
-- Migration: assistant/003_artifact_shares
-- Owner    : gateway / capability-worker
-- Purpose  : Schema-split artifact sharing with legacy quiz-link compatibility.
-- =============================================================================

-- A legacy root migration may have created the table before or after the schema
-- split. Adopt that table when present; otherwise create it in the owner schema.
DO $migrate$
BEGIN
    IF to_regclass('assistant.artifact_shares') IS NULL THEN
        IF to_regclass('public.artifact_shares') IS NOT NULL THEN
            ALTER TABLE public.artifact_shares SET SCHEMA assistant;
        ELSIF to_regclass('gateway.artifact_shares') IS NOT NULL THEN
            ALTER TABLE gateway.artifact_shares SET SCHEMA assistant;
        END IF;
    END IF;
END
$migrate$;

DO $migrate$
BEGIN
    IF to_regclass('assistant.artifact_share_submitters') IS NULL THEN
        IF to_regclass('public.artifact_share_submitters') IS NOT NULL THEN
            ALTER TABLE public.artifact_share_submitters SET SCHEMA assistant;
        ELSIF to_regclass('gateway.artifact_share_submitters') IS NOT NULL THEN
            ALTER TABLE gateway.artifact_share_submitters SET SCHEMA assistant;
        END IF;
    END IF;
END
$migrate$;

CREATE TABLE IF NOT EXISTS assistant.artifact_shares (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    share_code          VARCHAR(20) NOT NULL UNIQUE,
    kind                VARCHAR(32) NOT NULL DEFAULT 'quiz',
    title               TEXT NOT NULL DEFAULT '',
    payload             JSONB NOT NULL DEFAULT '{}'::jsonb,
    answer_keys         JSONB,
    tenant_id           VARCHAR(64),
    created_by          VARCHAR(128),
    is_active           BOOLEAN NOT NULL DEFAULT TRUE,
    max_attempts        INT,
    expires_at          TIMESTAMPTZ,
    require_name        BOOLEAN NOT NULL DEFAULT TRUE,
    time_limit_minutes  INT,
    attempt_count       INT NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at          TIMESTAMPTZ
);

-- 083 originally narrowed this to VARCHAR(12), but quiz_shares has always
-- allowed 20 characters. Widen before backfill so every valid legacy link fits.
ALTER TABLE assistant.artifact_shares
    ALTER COLUMN share_code TYPE VARCHAR(20);

CREATE INDEX IF NOT EXISTS idx_artifact_shares_tenant
    ON assistant.artifact_shares (tenant_id);
CREATE INDEX IF NOT EXISTS idx_artifact_shares_kind
    ON assistant.artifact_shares (kind);

INSERT INTO assistant.artifact_shares
    (id, share_code, kind, title, payload, answer_keys, tenant_id, created_by,
     is_active, max_attempts, expires_at, require_name, time_limit_minutes,
     attempt_count, created_at, revoked_at)
SELECT
    share.id,
    share.share_code,
    'quiz',
    quiz.title,
    jsonb_build_object(
        'quiz_id', share.quiz_id::text,
        'description', quiz.description,
        'question_count', quiz.question_count,
        'difficulty', quiz.difficulty,
        'questions', COALESCE((
            SELECT jsonb_agg(jsonb_build_object(
                'id', question.id::text,
                'question_num', question.question_num,
                'question_type', question.question_type,
                'question_text', question.question_text,
                'options', question.options
            ) ORDER BY question.question_num)
            FROM assistant.quiz_questions question
            WHERE question.quiz_id = share.quiz_id
        ), '[]'::jsonb)
    ),
    COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'id', question.id::text,
            'question_num', question.question_num,
            'correct_answer', question.correct_answer,
            'explanation', question.explanation
        ) ORDER BY question.question_num)
        FROM assistant.quiz_questions question
        WHERE question.quiz_id = share.quiz_id
    ), '[]'::jsonb),
    quiz.tenant_id,
    share.created_by,
    share.is_active,
    share.max_attempts,
    share.expires_at,
    share.require_name,
    share.time_limit_minutes,
    (
        SELECT count(*)::int
        FROM assistant.quiz_attempts attempt
        WHERE attempt.share_id = share.id
    ),
    share.created_at,
    NULL
FROM assistant.quiz_shares share
JOIN assistant.quizzes quiz ON quiz.id = share.quiz_id
ON CONFLICT (share_code) DO NOTHING;

-- A dedicated claim table gives display-name deduplication a database-owned
-- uniqueness boundary. Existing attempts are folded with DISTINCT.
CREATE TABLE IF NOT EXISTS assistant.artifact_share_submitters (
    share_id       UUID NOT NULL
                   REFERENCES assistant.artifact_shares(id) ON DELETE CASCADE,
    display_name   VARCHAR(100) NOT NULL,
    claimed_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (share_id, display_name)
);

INSERT INTO assistant.artifact_share_submitters (share_id, display_name)
SELECT DISTINCT attempt.share_id, LEFT(attempt.display_name, 100)
FROM assistant.quiz_attempts attempt
JOIN assistant.artifact_shares share ON share.id = attempt.share_id
WHERE attempt.share_id IS NOT NULL AND attempt.display_name IS NOT NULL
ON CONFLICT (share_id, display_name) DO NOTHING;
