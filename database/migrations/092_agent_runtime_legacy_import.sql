-- 092 - Transactional legacy-session import and native ThreadStore helpers.
--
-- Additive only.  sessions.history remains the V1 source/projection; this
-- migration makes a Agent-owned copy exactly once while keeping an in-flight
-- Python run, approval, or unknown side effect on its original kernel.

BEGIN;

ALTER TABLE assistant_runtime_threads
    ADD COLUMN IF NOT EXISTS dynamic_tool_fingerprint CHAR(64);

ALTER TABLE assistant_runtime_threads
    DROP CONSTRAINT IF EXISTS assistant_runtime_threads_dynamic_tool_fingerprint_check;
ALTER TABLE assistant_runtime_threads
    ADD CONSTRAINT assistant_runtime_threads_dynamic_tool_fingerprint_check
    CHECK (
        dynamic_tool_fingerprint IS NULL
        OR dynamic_tool_fingerprint ~ '^[0-9a-f]{64}$'
    );

CREATE OR REPLACE FUNCTION ensure_assistant_runtime_thread(
    p_runtime_thread_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_source_kind VARCHAR DEFAULT 'native'
)
RETURNS VOID AS $$
BEGIN
    IF p_source_kind NOT IN ('native', 'legacy_import') THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_SOURCE_KIND_INVALID'
            USING ERRCODE = '22023';
    END IF;

    INSERT INTO assistant_runtime_threads (
        runtime_thread_id, tenant_id, user_id, session_id,
        kernel_owner, source_kind, import_status
    ) VALUES (
        p_runtime_thread_id, p_tenant_id, p_user_id, p_session_id,
        'agent_runtime', p_source_kind,
        CASE WHEN p_source_kind = 'legacy_import' THEN 'pending' ELSE 'not_required' END
    ) ON CONFLICT (tenant_id, user_id, session_id) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1 FROM assistant_runtime_threads
         WHERE runtime_thread_id = p_runtime_thread_id
           AND tenant_id = p_tenant_id AND user_id = p_user_id
           AND session_id = p_session_id AND kernel_owner = 'agent_runtime'
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;

    INSERT INTO assistant_runtime_thread_members (
        kernel_thread_id, runtime_thread_id, kernel_session_id,
        relation_kind, tenant_id, user_id, session_id
    ) VALUES (
        p_runtime_thread_id, p_runtime_thread_id, p_runtime_thread_id,
        'root', p_tenant_id, p_user_id, p_session_id
    ) ON CONFLICT (kernel_thread_id) DO NOTHING;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION import_assistant_legacy_session(
    p_runtime_thread_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR
)
RETURNS TABLE(import_status VARCHAR, source_history_count INTEGER, source_history_sha256 CHAR(64)) AS $$
DECLARE
    v_history JSONB;
    v_message JSONB;
    v_index INTEGER;
    v_count INTEGER;
    v_hash CHAR(64);
    v_thread_status VARCHAR(32);
    v_event_id UUID;
    v_event_key VARCHAR(255);
BEGIN
    SELECT s.history
      INTO v_history
      FROM sessions s
     WHERE s.session_id = p_session_id
       AND s.tenant_id = p_tenant_id
       AND s.user_id = p_user_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_SESSION_NOT_FOUND' USING ERRCODE = '42501';
    END IF;

    -- A session must finish on its original kernel before ownership changes.
    IF EXISTS (
        SELECT 1 FROM assistant_runs
         WHERE tenant_id = p_tenant_id AND user_id = p_user_id
           AND session_id = p_session_id
           AND status IN ('running', 'awaiting_approval', 'approval_claimed',
                          'side_effect_unknown', 'terminal_persistence_unknown')
    ) OR (
        to_regclass('assistant_command_queue') IS NOT NULL
        AND EXISTS (
        SELECT 1 FROM assistant_command_queue
         WHERE tenant_id = p_tenant_id AND user_id = p_user_id
           AND session_id = p_session_id
           AND status IN ('queued', 'running', 'awaiting_approval',
                          'approval_claimed', 'side_effect_unknown')
        )
    ) OR (
        to_regclass('assistant_tool_approvals') IS NOT NULL
        AND EXISTS (
        SELECT 1 FROM assistant_tool_approvals
         WHERE tenant_id = p_tenant_id AND user_id = p_user_id
           AND session_id = p_session_id
           AND status IN ('pending', 'claimed')
        )
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_IN_FLIGHT' USING ERRCODE = '55000';
    END IF;

    v_history := CASE WHEN jsonb_typeof(v_history) = 'array' THEN v_history ELSE '[]'::jsonb END;
    v_count := jsonb_array_length(v_history);
    v_hash := encode(digest(v_history::text, 'sha256'), 'hex')::CHAR(64);

    PERFORM ensure_assistant_runtime_thread(
        p_runtime_thread_id, p_tenant_id, p_user_id, p_session_id, 'legacy_import'
    );

    SELECT t.import_status INTO v_thread_status
      FROM assistant_runtime_threads t
     WHERE t.runtime_thread_id = p_runtime_thread_id
       AND t.tenant_id = p_tenant_id AND t.user_id = p_user_id
       AND t.session_id = p_session_id
     FOR UPDATE;
    IF v_thread_status = 'ready' THEN
        RETURN QUERY SELECT v_thread_status, v_count, v_hash;
        RETURN;
    END IF;
    IF v_thread_status = 'importing' THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_IN_FLIGHT' USING ERRCODE = '55000';
    END IF;

    UPDATE assistant_runtime_threads
       SET import_status = 'importing', source_kind = 'legacy_import'
     WHERE runtime_thread_id = p_runtime_thread_id;

    FOR v_message, v_index IN
        SELECT value, ordinality::INTEGER
          FROM jsonb_array_elements(v_history) WITH ORDINALITY
    LOOP
        v_event_key := 'legacy:' || p_session_id || ':' || v_index::TEXT;
        v_event_id := md5(v_event_key)::UUID;
        IF jsonb_typeof(v_message) <> 'object' THEN
            v_message := jsonb_build_object('content', v_message);
        END IF;
        v_message := jsonb_build_object(
            'type', 'response_item',
            'payload', jsonb_build_object(
                'type', 'message',
                'role', COALESCE(v_message->>'role', 'user'),
                'content', jsonb_build_array(
                    jsonb_build_object(
                        'type', CASE WHEN COALESCE(v_message->>'role', 'user') = 'assistant'
                                     THEN 'output_text' ELSE 'input_text' END,
                        'text', COALESCE(v_message->>'content', v_message::text)
                    )
                )
            )
        );
        PERFORM append_assistant_runtime_item(
            p_runtime_thread_id, p_runtime_thread_id,
            p_tenant_id, p_user_id, p_session_id,
            v_event_id, v_event_key, 'legacy-' || v_index::TEXT,
            'legacy-' || v_index::TEXT, 'rollout/item', 'message', 'completed',
            v_message, encode(digest(v_message::text, 'sha256'), 'hex')::CHAR(64)
        );
    END LOOP;

    INSERT INTO assistant_runtime_thread_projections (
        kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id, projection
    ) VALUES (
        p_runtime_thread_id, p_runtime_thread_id, p_tenant_id, p_user_id, p_session_id,
        jsonb_build_object(
            'legacy_import', jsonb_build_object(
                'schema_version', 'assistant-turn-contract/v1',
                'source', 'legacy_session_history',
                'history_count', v_count,
                'history_sha256', v_hash
            )
        )
    ) ON CONFLICT (kernel_thread_id) DO UPDATE
        SET projection = assistant_runtime_thread_projections.projection
            || EXCLUDED.projection,
            updated_at = NOW();

    UPDATE assistant_runtime_threads
       SET import_status = 'ready', imported_at = NOW(),
           source_history_count = v_count, source_history_sha256 = v_hash,
           import_error_code = NULL
     WHERE runtime_thread_id = p_runtime_thread_id;

    RETURN QUERY SELECT 'ready'::VARCHAR, v_count, v_hash;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION import_assistant_legacy_session(UUID, VARCHAR, VARCHAR, VARCHAR) IS
    'Atomic, idempotent legacy history import. Refuses in-flight, approval, and unknown-side-effect sessions.';

COMMIT;
