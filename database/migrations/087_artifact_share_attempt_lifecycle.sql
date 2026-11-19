-- 087_artifact_share_attempt_lifecycle.sql
-- Per-attempt timing and atomic public quiz submission.

CREATE TABLE IF NOT EXISTS assistant.artifact_share_attempt_tokens (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    share_id    UUID NOT NULL
                REFERENCES assistant.artifact_shares(id) ON DELETE CASCADE,
    token_hash  CHAR(64) NOT NULL UNIQUE,
    started_at  TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (expires_at > started_at)
);

CREATE INDEX IF NOT EXISTS idx_artifact_share_attempt_tokens_share
    ON assistant.artifact_share_attempt_tokens (share_id, expires_at)
    WHERE consumed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_artifact_share_attempt_tokens_expiry
    ON assistant.artifact_share_attempt_tokens (expires_at);
CREATE INDEX IF NOT EXISTS idx_artifact_share_attempt_tokens_consumed
    ON assistant.artifact_share_attempt_tokens (consumed_at)
    WHERE consumed_at IS NOT NULL;

-- Custom SQLSTATE values are deliberately stable. The Python boundary maps
-- these codes to typed domain errors without parsing database error text.
CREATE OR REPLACE FUNCTION assistant.record_artifact_share_quiz_attempt(
    p_share_code VARCHAR,
    p_token_hash CHAR(64),
    p_display_name VARCHAR,
    p_attempt_id UUID,
    p_quiz_id UUID,
    p_answers JSONB,
    p_total_score DOUBLE PRECISION,
    p_correct_count INT,
    p_total_count INT,
    p_client_ip VARCHAR
)
RETURNS TABLE(attempt_id UUID, started_at TIMESTAMPTZ)
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = pg_catalog, assistant
AS $function$
DECLARE
    share_row assistant.artifact_shares%ROWTYPE;
    token_row assistant.artifact_share_attempt_tokens%ROWTYPE;
    claimed_rows INT := 0;
    attempt_started_at TIMESTAMPTZ := clock_timestamp();
BEGIN
    SELECT *
    INTO share_row
    FROM assistant.artifact_shares
    WHERE share_code = p_share_code
    FOR UPDATE;

    IF NOT FOUND
       OR NOT share_row.is_active
       OR (share_row.expires_at IS NOT NULL
           AND share_row.expires_at <= clock_timestamp()) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'P4040',
            MESSAGE = 'Share not found or expired';
    END IF;
    IF share_row.kind <> 'quiz' OR p_quiz_id IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'P4000',
            MESSAGE = 'Share is not a valid quiz';
    END IF;
    IF share_row.max_attempts IS NOT NULL
       AND share_row.attempt_count >= share_row.max_attempts THEN
        RAISE EXCEPTION USING
            ERRCODE = 'P4290',
            MESSAGE = 'Maximum attempts reached';
    END IF;
    IF share_row.require_name AND p_display_name IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'P4000',
            MESSAGE = 'This share requires a name before submitting';
    END IF;

    IF share_row.time_limit_minutes IS NOT NULL AND p_token_hash IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'P4000',
            MESSAGE = 'An attempt token is required for timed shares';
    END IF;

    IF p_token_hash IS NOT NULL THEN
        SELECT *
        INTO token_row
        FROM assistant.artifact_share_attempt_tokens
        WHERE token_hash = p_token_hash
        FOR UPDATE;

        IF NOT FOUND OR token_row.share_id <> share_row.id THEN
            RAISE EXCEPTION USING
                ERRCODE = 'P4000',
                MESSAGE = 'Attempt token is invalid or expired';
        END IF;
        IF token_row.consumed_at IS NOT NULL THEN
            RAISE EXCEPTION USING
                ERRCODE = 'P4090',
                MESSAGE = 'Attempt token has already been consumed';
        END IF;
        IF token_row.expires_at <= clock_timestamp() THEN
            RAISE EXCEPTION USING
                ERRCODE = 'P4000',
                MESSAGE = 'Attempt token is invalid or expired';
        END IF;
        attempt_started_at := token_row.started_at;
    END IF;

    IF p_display_name IS NOT NULL THEN
        INSERT INTO assistant.artifact_share_submitters (share_id, display_name)
        VALUES (share_row.id, p_display_name)
        ON CONFLICT (share_id, display_name) DO NOTHING;
        GET DIAGNOSTICS claimed_rows = ROW_COUNT;
        IF claimed_rows <> 1 THEN
            RAISE EXCEPTION USING
                ERRCODE = 'P4090',
                MESSAGE = 'This display name has already submitted';
        END IF;
    END IF;

    UPDATE assistant.artifact_shares
    SET attempt_count = attempt_count + 1
    WHERE id = share_row.id;

    IF p_token_hash IS NOT NULL THEN
        UPDATE assistant.artifact_share_attempt_tokens
        SET consumed_at = clock_timestamp()
        WHERE id = token_row.id;
    END IF;

    INSERT INTO assistant.quiz_attempts (
        id, quiz_id, user_id, share_id, display_name, answers,
        total_score, correct_count, total_count, started_at, completed_at,
        status, client_ip, exam_id
    )
    VALUES (
        p_attempt_id, p_quiz_id, NULL, share_row.id, p_display_name, p_answers,
        p_total_score, p_correct_count, p_total_count, attempt_started_at,
        clock_timestamp(), 'completed', p_client_ip, NULL
    );

    RETURN QUERY SELECT p_attempt_id, attempt_started_at;
END
$function$;
