-- 083_artifact_shares.sql
-- Kind-generic public sharing of agent artifacts (product-convergence PC-03).
-- Supersedes quiz_shares: legacy quiz shares are backfilled with a frozen
-- payload/answer_keys snapshot; the public /quiz/shared/* routes now read
-- artifact_shares with kind='quiz'. quiz_shares itself is kept for history.

CREATE TABLE IF NOT EXISTS artifact_shares (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Keep the legacy width so existing 13-20 character links can be backfilled.
    share_code        VARCHAR(20) NOT NULL UNIQUE,
    kind              VARCHAR(32) NOT NULL DEFAULT 'quiz',
    title             TEXT NOT NULL DEFAULT '',
    payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    answer_keys       JSONB,
    tenant_id         VARCHAR(64),
    created_by        VARCHAR(128),
    is_active         BOOLEAN NOT NULL DEFAULT TRUE,
    max_attempts      INT,
    expires_at        TIMESTAMPTZ,
    require_name      BOOLEAN NOT NULL DEFAULT TRUE,
    time_limit_minutes INT,
    attempt_count     INT NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_artifact_shares_tenant ON artifact_shares (tenant_id);
CREATE INDEX IF NOT EXISTS idx_artifact_shares_kind ON artifact_shares (kind);
ALTER TABLE artifact_shares ALTER COLUMN share_code TYPE VARCHAR(20);

-- Backfill legacy quiz shares as kind='quiz' artifact shares with a frozen
-- snapshot so old links keep working even if the source quiz changes later.
INSERT INTO artifact_shares
    (id, share_code, kind, title, payload, answer_keys, tenant_id, created_by,
     is_active, max_attempts, expires_at, require_name, time_limit_minutes,
     attempt_count, created_at, revoked_at)
SELECT
    s.id,
    s.share_code,
    'quiz',
    q.title,
    jsonb_build_object(
        'quiz_id', s.quiz_id::text,
        'description', q.description,
        'question_count', q.question_count,
        'difficulty', q.difficulty,
        'questions', COALESCE((
            SELECT jsonb_agg(jsonb_build_object(
                'id', qq.id::text,
                'question_num', qq.question_num,
                'question_type', qq.question_type,
                'question_text', qq.question_text,
                'options', qq.options
            ) ORDER BY qq.question_num)
            FROM quiz_questions qq WHERE qq.quiz_id = s.quiz_id
        ), '[]'::jsonb)
    ),
    COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'id', qq.id::text,
            'question_num', qq.question_num,
            'correct_answer', qq.correct_answer,
            'explanation', qq.explanation
        ) ORDER BY qq.question_num)
        FROM quiz_questions qq WHERE qq.quiz_id = s.quiz_id
    ), '[]'::jsonb),
    q.tenant_id,
    s.created_by,
    s.is_active,
    s.max_attempts,
    s.expires_at,
    s.require_name,
    s.time_limit_minutes,
    (SELECT count(*) FROM quiz_attempts qa WHERE qa.share_id = s.id),
    s.created_at,
    NULL
FROM quiz_shares s
JOIN quizzes q ON q.id = s.quiz_id
ON CONFLICT (share_code) DO NOTHING;

-- Atomic display-name claims close the SELECT-then-INSERT race in anonymous
-- submissions without rewriting historical attempt rows.
CREATE TABLE IF NOT EXISTS artifact_share_submitters (
    share_id       UUID NOT NULL REFERENCES artifact_shares(id) ON DELETE CASCADE,
    display_name   VARCHAR(100) NOT NULL,
    claimed_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (share_id, display_name)
);

INSERT INTO artifact_share_submitters (share_id, display_name)
SELECT DISTINCT qa.share_id, LEFT(qa.display_name, 100)
FROM quiz_attempts qa
JOIN artifact_shares s ON s.id = qa.share_id
WHERE qa.share_id IS NOT NULL AND qa.display_name IS NOT NULL
ON CONFLICT (share_id, display_name) DO NOTHING;
