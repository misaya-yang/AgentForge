-- 094 - Lossless, schema-safe legacy Assistant history import.
--
-- Migration 092 established the atomic import boundary, but its unqualified
-- source-table references resolve to gateway.* first after the schema split.
-- The authoritative legacy session, Run, command, and approval records live
-- in assistant.*.  It also flattened persisted tool calls/results into the
-- final Assistant message.  This replacement keeps the transaction and
-- source receipt while emitting paired Agent response items and durable
-- approval receipts.  Existing ready imports remain immutable.

BEGIN;

CREATE OR REPLACE FUNCTION import_assistant_legacy_session(
    p_runtime_thread_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR
)
RETURNS TABLE(import_status VARCHAR, source_history_count INTEGER, source_history_sha256 CHAR(64)) AS $$
DECLARE
    v_sessions_table REGCLASS;
    v_runs_table REGCLASS;
    v_commands_table REGCLASS;
    v_approvals_table REGCLASS;
    v_session_row JSONB;
    v_history JSONB;
    v_message JSONB;
    v_metadata JSONB;
    v_tool_calls JSONB;
    v_tool_results JSONB;
    v_tool_call JSONB;
    v_tool_result JSONB;
    v_approval JSONB;
    v_index INTEGER;
    v_tool_index INTEGER;
    v_count INTEGER;
    v_rollout_count INTEGER := 0;
    v_tool_call_count INTEGER := 0;
    v_approval_count INTEGER := 0;
    v_hash CHAR(64);
    v_stored_count INTEGER;
    v_stored_hash CHAR(64);
    v_thread_status VARCHAR(32);
    v_role TEXT;
    v_content TEXT;
    v_call_id TEXT;
    v_tool_name TEXT;
    v_arguments JSONB;
    v_arguments_text TEXT;
    v_output_text TEXT;
    v_seen_call_ids TEXT[] := ARRAY[]::TEXT[];
    v_message_call_ids TEXT[] := ARRAY[]::TEXT[];
    v_payload JSONB;
    v_event_id UUID;
    v_event_key VARCHAR(255);
    v_event_prefix TEXT;
    v_in_flight BOOLEAN := FALSE;
BEGIN
    -- Prefer the authoritative split-schema tables.  The unqualified fallback
    -- keeps isolated single-schema migration contracts representative.
    -- A test or embedded deployment may intentionally install the function in
    -- its own first search_path schema while an unrelated assistant schema is
    -- present in the same database.  Only the production gateway-owned
    -- function crosses into assistant.* explicitly.
    v_sessions_table := CASE WHEN current_schema() = 'gateway'
        THEN COALESCE(to_regclass('assistant.sessions'), to_regclass('sessions'))
        ELSE to_regclass('sessions') END;
    v_runs_table := CASE WHEN current_schema() = 'gateway'
        THEN COALESCE(to_regclass('assistant.assistant_runs'), to_regclass('assistant_runs'))
        ELSE to_regclass('assistant_runs') END;
    v_commands_table := CASE WHEN current_schema() = 'gateway'
        THEN COALESCE(to_regclass('assistant.assistant_command_queue'), to_regclass('assistant_command_queue'))
        ELSE to_regclass('assistant_command_queue') END;
    v_approvals_table := CASE WHEN current_schema() = 'gateway'
        THEN COALESCE(to_regclass('assistant.assistant_tool_approvals'), to_regclass('assistant_tool_approvals'))
        ELSE to_regclass('assistant_tool_approvals') END;

    IF v_sessions_table IS NULL THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_SESSION_STORAGE_UNAVAILABLE'
            USING ERRCODE = '55000';
    END IF;

    EXECUTE format(
        'SELECT jsonb_build_object(''history'', COALESCE(history, ''[]''::jsonb)) '
        'FROM %s WHERE session_id = $1 AND tenant_id = $2 AND user_id = $3 FOR UPDATE',
        v_sessions_table
    )
    INTO v_session_row
    USING p_session_id, p_tenant_id, p_user_id;

    IF v_session_row IS NULL THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_SESSION_NOT_FOUND' USING ERRCODE = '42501';
    END IF;

    IF v_runs_table IS NOT NULL THEN
        EXECUTE format(
            'SELECT EXISTS (SELECT 1 FROM %s '
            'WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 '
            'AND status IN (''running'', ''blocked'', ''awaiting_approval'', ''approval_claimed'', '
            '''side_effect_unknown'', ''terminal_persistence_unknown''))',
            v_runs_table
        )
        INTO v_in_flight
        USING p_tenant_id, p_user_id, p_session_id;
    END IF;

    IF NOT v_in_flight AND v_commands_table IS NOT NULL THEN
        EXECUTE format(
            'SELECT EXISTS (SELECT 1 FROM %s '
            'WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 '
            'AND status IN (''queued'', ''running'', ''awaiting_approval'', '
            '''approval_claimed'', ''side_effect_unknown''))',
            v_commands_table
        )
        INTO v_in_flight
        USING p_tenant_id, p_user_id, p_session_id;
    END IF;

    IF NOT v_in_flight AND v_approvals_table IS NOT NULL THEN
        EXECUTE format(
            'SELECT EXISTS (SELECT 1 FROM %s '
            'WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 '
            'AND status IN (''pending'', ''claimed'', ''approved''))',
            v_approvals_table
        )
        INTO v_in_flight
        USING p_tenant_id, p_user_id, p_session_id;
    END IF;

    IF v_in_flight THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_IN_FLIGHT' USING ERRCODE = '55000';
    END IF;

    v_history := v_session_row->'history';
    v_history := CASE WHEN jsonb_typeof(v_history) = 'array' THEN v_history ELSE '[]'::jsonb END;
    v_count := jsonb_array_length(v_history);
    v_hash := encode(digest(v_history::text, 'sha256'), 'hex')::CHAR(64);
    v_event_prefix := 'legacy:' || md5(p_session_id);

    PERFORM ensure_assistant_runtime_thread(
        p_runtime_thread_id, p_tenant_id, p_user_id, p_session_id, 'legacy_import'
    );

    SELECT t.import_status, t.source_history_count, t.source_history_sha256
      INTO v_thread_status, v_stored_count, v_stored_hash
      FROM assistant_runtime_threads t
     WHERE t.runtime_thread_id = p_runtime_thread_id
       AND t.tenant_id = p_tenant_id AND t.user_id = p_user_id
       AND t.session_id = p_session_id
     FOR UPDATE;
    IF v_thread_status = 'ready' THEN
        IF v_stored_count <> v_count OR v_stored_hash IS DISTINCT FROM v_hash THEN
            RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_SOURCE_CHANGED'
                USING ERRCODE = '55000';
        END IF;
        RETURN QUERY SELECT v_thread_status, v_stored_count, v_stored_hash;
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
        IF jsonb_typeof(v_message) <> 'object' THEN
            v_message := jsonb_build_object('role', 'user', 'content', v_message);
        END IF;
        v_role := COALESCE(NULLIF(v_message->>'role', ''), 'user');
        IF v_role NOT IN ('user', 'assistant') THEN
            RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_ROLE_INVALID'
                USING ERRCODE = '22023';
        END IF;
        v_content := CASE
            WHEN v_message->'content' IS NULL OR v_message->'content' = 'null'::jsonb THEN ''
            WHEN jsonb_typeof(v_message->'content') = 'string' THEN v_message->>'content'
            ELSE (v_message->'content')::text
        END;
        v_metadata := CASE
            WHEN jsonb_typeof(v_message->'metadata') = 'object' THEN v_message->'metadata'
            ELSE '{}'::jsonb
        END;
        v_tool_calls := CASE
            WHEN jsonb_typeof(v_metadata->'tool_calls') = 'array' THEN v_metadata->'tool_calls'
            ELSE '[]'::jsonb
        END;
        v_tool_results := CASE
            WHEN jsonb_typeof(v_metadata->'tool_results') = 'array' THEN v_metadata->'tool_results'
            ELSE '[]'::jsonb
        END;
        v_message_call_ids := ARRAY[]::TEXT[];

        IF v_role <> 'assistant'
           AND (jsonb_array_length(v_tool_calls) > 0 OR jsonb_array_length(v_tool_results) > 0) THEN
            RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                USING ERRCODE = '22023';
        END IF;

        IF v_role = 'assistant' THEN
            FOR v_tool_call, v_tool_index IN
                SELECT value, ordinality::INTEGER
                  FROM jsonb_array_elements(v_tool_calls) WITH ORDINALITY
            LOOP
                IF jsonb_typeof(v_tool_call) <> 'object' THEN
                    RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                        USING ERRCODE = '22023';
                END IF;
                v_call_id := NULLIF(v_tool_call->>'id', '');
                v_tool_name := NULLIF(v_tool_call->>'name', '');
                IF v_call_id IS NULL OR v_tool_name IS NULL OR v_call_id = ANY(v_seen_call_ids) THEN
                    RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                        USING ERRCODE = '22023';
                END IF;
                IF (
                    SELECT count(*) FROM jsonb_array_elements(v_tool_calls) AS call
                    WHERE call->>'id' = v_call_id
                ) <> 1 OR (
                    SELECT count(*) FROM jsonb_array_elements(v_tool_results) AS result
                    WHERE result->>'tool_call_id' = v_call_id
                ) <> 1 THEN
                    RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                        USING ERRCODE = '22023';
                END IF;
                SELECT value INTO v_tool_result
                  FROM jsonb_array_elements(v_tool_results)
                 WHERE value->>'tool_call_id' = v_call_id;
                IF jsonb_typeof(v_tool_result) <> 'object' THEN
                    RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                        USING ERRCODE = '22023';
                END IF;

                v_arguments := COALESCE(v_tool_call->'arguments', '{}'::jsonb);
                IF jsonb_typeof(v_arguments) <> 'object' THEN
                    RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_ARGUMENTS_INVALID'
                        USING ERRCODE = '22023';
                END IF;
                v_arguments_text := v_arguments::text;
                v_seen_call_ids := array_append(v_seen_call_ids, v_call_id);
                v_message_call_ids := array_append(v_message_call_ids, v_call_id);

                v_payload := jsonb_build_object(
                    'type', 'response_item',
                    'payload', jsonb_build_object(
                        'type', 'function_call',
                        'name', v_tool_name,
                        'arguments', v_arguments_text,
                        'call_id', v_call_id
                    )
                );
                v_event_key := v_event_prefix || ':' || v_index::TEXT
                    || ':tool:' || v_tool_index::TEXT || ':call';
                v_event_id := md5(v_event_key)::UUID;
                PERFORM append_assistant_runtime_item(
                    p_runtime_thread_id, p_runtime_thread_id,
                    p_tenant_id, p_user_id, p_session_id,
                    v_event_id, v_event_key, 'legacy-' || v_index::TEXT,
                    v_call_id, 'rollout/item', 'function_call', 'completed',
                    v_payload, encode(digest(v_payload::text, 'sha256'), 'hex')::CHAR(64)
                );
                v_rollout_count := v_rollout_count + 1;

                v_output_text := CASE
                    WHEN NULLIF(v_tool_result->>'error', '') IS NOT NULL THEN
                        jsonb_build_object(
                            'status', COALESCE(v_tool_result->>'status', 'error'),
                            'result', v_tool_result->'result',
                            'error', v_tool_result->>'error'
                        )::text
                    WHEN v_tool_result->'result' IS NULL
                         OR v_tool_result->'result' = 'null'::jsonb THEN 'null'
                    WHEN jsonb_typeof(v_tool_result->'result') = 'string' THEN
                        v_tool_result->>'result'
                    ELSE (v_tool_result->'result')::text
                END;
                v_payload := jsonb_build_object(
                    'type', 'response_item',
                    'payload', jsonb_build_object(
                        'type', 'function_call_output',
                        'call_id', v_call_id,
                        'name', v_tool_name,
                        'output', v_output_text
                    )
                );
                v_event_key := v_event_prefix || ':' || v_index::TEXT
                    || ':tool:' || v_tool_index::TEXT || ':result';
                v_event_id := md5(v_event_key)::UUID;
                PERFORM append_assistant_runtime_item(
                    p_runtime_thread_id, p_runtime_thread_id,
                    p_tenant_id, p_user_id, p_session_id,
                    v_event_id, v_event_key, 'legacy-' || v_index::TEXT,
                    v_call_id, 'rollout/item', 'function_call_output', 'completed',
                    v_payload, encode(digest(v_payload::text, 'sha256'), 'hex')::CHAR(64)
                );
                v_rollout_count := v_rollout_count + 1;
                v_tool_call_count := v_tool_call_count + 1;
            END LOOP;

            IF EXISTS (
                SELECT 1 FROM jsonb_array_elements(v_tool_results) AS result
                WHERE NULLIF(result->>'tool_call_id', '') IS NULL
                   OR NOT (result->>'tool_call_id' = ANY(v_message_call_ids))
            ) THEN
                RAISE EXCEPTION 'ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID'
                    USING ERRCODE = '22023';
            END IF;
        END IF;

        -- Empty Assistant messages that only carried tool activity are not
        -- valid model inputs and add no information after the paired items.
        IF v_role = 'user' OR v_content <> '' THEN
            v_payload := jsonb_build_object(
                'type', 'response_item',
                'payload', jsonb_build_object(
                    'type', 'message',
                    'role', v_role,
                    'content', jsonb_build_array(
                        jsonb_build_object(
                            'type', CASE WHEN v_role = 'assistant'
                                         THEN 'output_text' ELSE 'input_text' END,
                            'text', v_content
                        )
                    )
                )
            );
            v_event_key := v_event_prefix || ':' || v_index::TEXT || ':message';
            v_event_id := md5(v_event_key)::UUID;
            PERFORM append_assistant_runtime_item(
                p_runtime_thread_id, p_runtime_thread_id,
                p_tenant_id, p_user_id, p_session_id,
                v_event_id, v_event_key, 'legacy-' || v_index::TEXT,
                'legacy-' || v_index::TEXT, 'rollout/item', 'message', 'completed',
                v_payload, encode(digest(v_payload::text, 'sha256'), 'hex')::CHAR(64)
            );
            v_rollout_count := v_rollout_count + 1;
        END IF;
    END LOOP;

    -- Preserve terminal approval provenance without replaying approval state or
    -- copying raw arguments into the model-visible history.
    IF v_approvals_table IS NOT NULL THEN
        FOR v_approval IN EXECUTE format(
            'SELECT to_jsonb(a) FROM %s a '
            'WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 '
            'AND status NOT IN (''pending'', ''claimed'', ''approved'') '
            'ORDER BY created_at, approval_id',
            v_approvals_table
        ) USING p_tenant_id, p_user_id, p_session_id
        LOOP
            v_payload := jsonb_build_object(
                'schema_version', 'agent-runtime-legacy-approval/v1',
                'approval_id', v_approval->>'approval_id',
                'run_id', v_approval->>'run_id',
                'tool_name', v_approval->>'tool_name',
                'status', v_approval->>'status',
                'reason', v_approval->>'reason',
                'approved_by', v_approval->>'approved_by',
                'approved_at', v_approval->>'approved_at',
                'expires_at', v_approval->>'expires_at',
                'arguments_sha256', encode(
                    digest(COALESCE(v_approval->'arguments', '{}'::jsonb)::text, 'sha256'),
                    'hex'
                )
            );
            v_event_key := v_event_prefix || ':approval:' || (v_approval->>'approval_id');
            v_event_id := md5(v_event_key)::UUID;
            PERFORM append_assistant_runtime_item(
                p_runtime_thread_id, p_runtime_thread_id,
                p_tenant_id, p_user_id, p_session_id,
                v_event_id, v_event_key, v_approval->>'run_id',
                v_approval->>'approval_id', 'agent-runtime/legacy_approval', 'approval',
                v_approval->>'status', v_payload,
                encode(digest(v_payload::text, 'sha256'), 'hex')::CHAR(64)
            );
            v_approval_count := v_approval_count + 1;
        END LOOP;
    END IF;

    INSERT INTO assistant_runtime_thread_projections (
        kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id, projection
    ) VALUES (
        p_runtime_thread_id, p_runtime_thread_id, p_tenant_id, p_user_id, p_session_id,
        jsonb_build_object(
            'legacy_import', jsonb_build_object(
                'schema_version', 'assistant-turn-contract/v1',
                'normalizer_version', 2,
                'source', 'legacy_session_history',
                'history_count', v_count,
                'history_sha256', v_hash,
                'rollout_item_count', v_rollout_count,
                'tool_call_count', v_tool_call_count,
                'approval_receipt_count', v_approval_count
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
$$ LANGUAGE plpgsql
SET search_path FROM CURRENT;

COMMENT ON FUNCTION import_assistant_legacy_session(UUID, VARCHAR, VARCHAR, VARCHAR) IS
    'Atomic schema-safe legacy import with strict tool pairing and hashed approval receipts.';

COMMIT;
