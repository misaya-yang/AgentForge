-- R1 AR-04: retain the exact graded response for a stable attempt id.
-- Additive: existing attempts remain valid; new submissions persist result_payload.

ALTER TABLE assistant.quiz_attempts
    ADD COLUMN IF NOT EXISTS result_payload JSONB;

ALTER TABLE assistant.quiz_attempts
    ADD COLUMN IF NOT EXISTS attempt_token_hash CHAR(64);

CREATE UNIQUE INDEX IF NOT EXISTS idx_quiz_attempts_share_token_once
    ON assistant.quiz_attempts (share_id, attempt_token_hash)
    WHERE attempt_token_hash IS NOT NULL;

-- Keep the v1 function for compatibility; the v2 function stores and replays
-- the exact result atomically with the attempt counter and one-use token.
CREATE OR REPLACE FUNCTION assistant.record_artifact_share_quiz_attempt_v2(
    p_share_code VARCHAR,
    p_token_hash CHAR(64),
    p_display_name VARCHAR,
    p_attempt_id UUID,
    p_quiz_id UUID,
    p_answers JSONB,
    p_total_score DOUBLE PRECISION,
    p_correct_count INT,
    p_total_count INT,
    p_client_ip VARCHAR,
    p_result JSONB
)
RETURNS TABLE(attempt_id UUID, result_payload JSONB)
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = pg_catalog, assistant
AS $function$
DECLARE
    share_row assistant.artifact_shares%ROWTYPE;
    token_row assistant.artifact_share_attempt_tokens%ROWTYPE;
    previous assistant.quiz_attempts%ROWTYPE;
    claimed_rows INT := 0;
    attempt_started_at TIMESTAMPTZ := clock_timestamp();
BEGIN
    SELECT * INTO share_row
    FROM assistant.artifact_shares
    WHERE share_code = p_share_code
    FOR UPDATE;
    IF NOT FOUND OR NOT share_row.is_active
       OR (share_row.expires_at IS NOT NULL AND share_row.expires_at <= clock_timestamp()) THEN
        RAISE EXCEPTION USING ERRCODE = 'P4040', MESSAGE = 'Share not found or expired';
    END IF;
    IF share_row.kind <> 'quiz' OR p_quiz_id IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'P4000', MESSAGE = 'Share is not a valid quiz';
    END IF;
    IF share_row.require_name AND p_display_name IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'P4000', MESSAGE = 'This share requires a name before submitting';
    END IF;
    IF p_token_hash IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'P4000', MESSAGE = 'Attempt token is required';
    END IF;

    SELECT * INTO token_row
    FROM assistant.artifact_share_attempt_tokens
    WHERE token_hash = p_token_hash
    FOR UPDATE;
    IF NOT FOUND OR token_row.share_id <> share_row.id THEN
        RAISE EXCEPTION USING ERRCODE = 'P4000', MESSAGE = 'Attempt token is invalid or expired';
    END IF;

    SELECT * INTO previous
    FROM assistant.quiz_attempts
    WHERE share_id = share_row.id AND attempt_token_hash = p_token_hash
    LIMIT 1;
    IF FOUND THEN
        IF previous.quiz_id <> p_quiz_id OR previous.answers <> p_answers
           OR previous.display_name IS DISTINCT FROM p_display_name THEN
            RAISE EXCEPTION USING ERRCODE = 'P4090', MESSAGE = 'Attempt token belongs to another answer';
        END IF;
        RETURN QUERY SELECT previous.id, previous.result_payload;
        RETURN;
    END IF;
    IF token_row.consumed_at IS NOT NULL OR token_row.expires_at <= clock_timestamp() THEN
        RAISE EXCEPTION USING ERRCODE = 'P4090', MESSAGE = 'Attempt token is no longer available';
    END IF;
    IF share_row.max_attempts IS NOT NULL AND share_row.attempt_count >= share_row.max_attempts THEN
        RAISE EXCEPTION USING ERRCODE = 'P4290', MESSAGE = 'Maximum attempts reached';
    END IF;
    attempt_started_at := token_row.started_at;

    IF p_display_name IS NOT NULL THEN
        INSERT INTO assistant.artifact_share_submitters (share_id, display_name)
        VALUES (share_row.id, p_display_name)
        ON CONFLICT (share_id, display_name) DO NOTHING;
        GET DIAGNOSTICS claimed_rows = ROW_COUNT;
        IF claimed_rows <> 1 THEN
            RAISE EXCEPTION USING ERRCODE = 'P4090', MESSAGE = 'This display name has already submitted';
        END IF;
    END IF;

    UPDATE assistant.artifact_shares
    SET attempt_count = attempt_count + 1
    WHERE id = share_row.id;
    UPDATE assistant.artifact_share_attempt_tokens
    SET consumed_at = clock_timestamp()
    WHERE id = token_row.id;

    INSERT INTO assistant.quiz_attempts (
        id, quiz_id, user_id, share_id, display_name, answers,
        total_score, correct_count, total_count, started_at, completed_at,
        status, client_ip, exam_id, result_payload, attempt_token_hash
    ) VALUES (
        p_attempt_id, p_quiz_id, NULL, share_row.id, p_display_name, p_answers,
        p_total_score, p_correct_count, p_total_count, attempt_started_at,
        clock_timestamp(), 'completed', p_client_ip, NULL, p_result, p_token_hash
    );
    RETURN QUERY SELECT p_attempt_id, p_result;
END
$function$;
