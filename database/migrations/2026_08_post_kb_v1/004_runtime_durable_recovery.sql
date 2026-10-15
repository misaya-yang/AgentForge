-- Rust owns durable execution; Gateway retains model/permission governance.
CREATE TABLE assistant.assistant_runtime_execution_owners (
    run_id UUID PRIMARY KEY REFERENCES assistant.assistant_runs(run_id),
    runtime_thread_id UUID NOT NULL REFERENCES assistant.assistant_runtime_threads(runtime_thread_id),
    owner_id UUID NOT NULL,
    fence BIGINT NOT NULL CHECK (fence > 0),
    lease_until TIMESTAMPTZ NOT NULL,
    context JSONB NOT NULL CHECK (jsonb_typeof(context) = 'object'),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE assistant.assistant_runtime_invocations (
    run_id UUID NOT NULL REFERENCES assistant.assistant_runs(run_id),
    kernel_thread_id UUID NOT NULL,
    call_id TEXT NOT NULL,
    params JSONB NOT NULL CHECK (jsonb_typeof(params) = 'object'),
    approval_id UUID REFERENCES assistant.assistant_tool_approvals(approval_id),
    response JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (run_id, kernel_thread_id, call_id),
    CHECK (response IS NULL OR jsonb_typeof(response) = 'object')
);

CREATE FUNCTION assistant.claim_runtime_execution(p_run UUID, p_owner UUID, p_context JSONB DEFAULT NULL)
RETURNS assistant.assistant_runtime_execution_owners
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, assistant AS $$
DECLARE
    r assistant.assistant_runs%ROWTYPE;
    claim assistant.assistant_runtime_execution_owners%ROWTYPE;
BEGIN
    SELECT * INTO r FROM assistant_runs WHERE run_id=p_run FOR UPDATE;
    IF NOT FOUND OR r.engine <> 'agent_runtime' OR r.status NOT IN ('running','awaiting_approval') THEN
        RAISE EXCEPTION 'RUNTIME_RECOVERY_NOT_ACTIVE' USING ERRCODE='55000';
    END IF;
    PERFORM 1 FROM assistant_runtime_threads WHERE runtime_thread_id=r.harness_thread_id AND deleted_at IS NULL FOR UPDATE;
    IF NOT FOUND OR NOT EXISTS (
        SELECT 1 FROM assistant_runtime_model_leases l JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
        WHERE l.run_id=p_run AND l.status='active' AND l.expires_at>clock_timestamp()
        AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
        AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=s.snapshot_id)
    ) THEN
        RAISE EXCEPTION 'RUNTIME_RECOVERY_AUTHORITY_EXPIRED' USING ERRCODE='42501';
    END IF;
    IF EXISTS (SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs other ON other.run_id=o.run_id
        WHERE o.runtime_thread_id=r.harness_thread_id AND o.run_id<>p_run AND other.status IN ('running','awaiting_approval')) THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_ALREADY_EXECUTING' USING ERRCODE='55000';
    END IF;
    SELECT * INTO claim FROM assistant_runtime_execution_owners WHERE run_id=p_run FOR UPDATE;
    IF FOUND THEN
        IF claim.owner_id=p_owner OR claim.lease_until>clock_timestamp() THEN
            RAISE EXCEPTION 'RUNTIME_EXECUTION_ALREADY_OWNED' USING ERRCODE='55000';
        END IF;
        IF p_context IS NOT NULL AND p_context <> claim.context THEN
            RAISE EXCEPTION 'RUNTIME_RECOVERY_CONTEXT_MISMATCH' USING ERRCODE='42501';
        END IF;
        UPDATE assistant_runtime_execution_owners SET owner_id=p_owner, fence=fence+1,
            lease_until=clock_timestamp()+INTERVAL '30 seconds', heartbeat_at=clock_timestamp()
            WHERE run_id=p_run RETURNING * INTO claim;
    ELSE
        IF p_context IS NULL THEN
            RAISE EXCEPTION 'RUNTIME_RECOVERY_CONTEXT_MISSING' USING ERRCODE='55000';
        END IF;
        INSERT INTO assistant_runtime_execution_owners(run_id,runtime_thread_id,owner_id,fence,lease_until,context)
        VALUES(p_run,r.harness_thread_id,p_owner,1,clock_timestamp()+INTERVAL '30 seconds',p_context) RETURNING * INTO claim;
    END IF;
    RETURN claim;
END $$;

CREATE FUNCTION assistant.assert_runtime_execution_fence(p_run UUID, p_owner UUID, p_fence BIGINT)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, assistant AS $$
BEGIN
    PERFORM 1 FROM assistant_runtime_execution_owners WHERE run_id=p_run AND owner_id=p_owner
        AND fence=p_fence AND lease_until>clock_timestamp() FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'RUNTIME_EXECUTION_FENCE_LOST' USING ERRCODE='42501';
    END IF;
    RETURN TRUE;
END $$;

-- Execution admission is stricter than receipt persistence: cancellation or
-- revocation blocks new work immediately while original receipts may still land.
CREATE FUNCTION assistant.assert_runtime_execution(p_run UUID,p_owner UUID,p_fence BIGINT)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    PERFORM 1 FROM assistant_runs r JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
        JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
        WHERE r.run_id=p_run AND r.status IN ('running','awaiting_approval') AND l.status='active' AND l.expires_at>clock_timestamp()
        AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
        AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=s.snapshot_id)
        FOR SHARE OF r,l,s;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_EXECUTION_AUTHORITY_REVOKED' USING ERRCODE='42501'; END IF;
    -- Claim, admission and cancellation lock the run before its execution owner.
    PERFORM assert_runtime_execution_fence(p_run,p_owner,p_fence);
    RETURN TRUE;
END $$;

-- Worker requests carry the execution owner proof. Hold the scoped run and
-- owner locks until reservation/cancellation commits, not only until validation.
CREATE FUNCTION assistant.assert_worker_execution_owner(p_run UUID,p_tenant VARCHAR,p_user VARCHAR,p_session VARCHAR,p_owner UUID,p_fence BIGINT,p_admission BOOLEAN)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    PERFORM 1 FROM assistant_runs WHERE run_id=p_run AND tenant_id=p_tenant
        AND user_id=p_user AND session_id=p_session FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_WORKER_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
    IF EXISTS (SELECT 1 FROM assistant_runtime_execution_owners WHERE run_id=p_run) THEN
        IF p_admission THEN PERFORM assert_runtime_execution(p_run,p_owner,p_fence);
        ELSE PERFORM assert_runtime_execution_fence(p_run,p_owner,p_fence); END IF;
    END IF;
    RETURN TRUE;
END $$;

CREATE FUNCTION assistant.cancel_runtime_execution(p_run UUID,p_thread UUID,p_tenant VARCHAR,p_user VARCHAR,p_session VARCHAR)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE changed BIGINT;
BEGIN
    PERFORM 1 FROM assistant_runs WHERE run_id=p_run AND harness_thread_id=p_thread
        AND tenant_id=p_tenant AND user_id=p_user AND session_id=p_session FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_CANCEL_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
    UPDATE assistant_runs SET status='cancelled',finished_at=COALESCE(finished_at,clock_timestamp()),updated_at=clock_timestamp()
        WHERE run_id=p_run AND status IN ('running','awaiting_approval');
    GET DIAGNOSTICS changed=ROW_COUNT;
    IF changed=0 THEN RETURN FALSE; END IF;
    UPDATE assistant_runtime_model_leases SET status='revoked',revoked_at=clock_timestamp() WHERE run_id=p_run AND status='active';
    UPDATE assistant_tool_approvals SET status='cancelled',reason='runtime_cancelled',approved_at=clock_timestamp()
        WHERE run_id=p_run AND status IN ('pending','approved');
    RETURN TRUE;
END $$;

-- Revocation and new admissions serialize on the original run. An absent
-- revocation row alone cannot be locked by an admission predicate.
CREATE FUNCTION assistant.lock_runtime_revocation() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    PERFORM 1 FROM assistant_runs r JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
        WHERE l.snapshot_id=NEW.snapshot_id ORDER BY r.run_id FOR UPDATE OF r;
    RETURN NEW;
END $$;
CREATE TRIGGER runtime_revocation_lock BEFORE INSERT ON assistant.assistant_runtime_snapshot_revocations
    FOR EACH ROW EXECUTE FUNCTION assistant.lock_runtime_revocation();

CREATE FUNCTION assistant.guard_runtime_invocation() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, assistant AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.params <> NEW.params OR OLD.run_id <> NEW.run_id
        OR OLD.kernel_thread_id <> NEW.kernel_thread_id OR OLD.call_id <> NEW.call_id
        OR (OLD.approval_id IS NOT NULL AND OLD.approval_id IS DISTINCT FROM NEW.approval_id)
        OR (OLD.response IS NOT NULL AND OLD.response IS DISTINCT FROM NEW.response)) THEN
        RAISE EXCEPTION 'RUNTIME_INVOCATION_IMMUTABLE' USING ERRCODE='55000';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER runtime_invocation_immutable BEFORE UPDATE ON assistant.assistant_runtime_invocations
    FOR EACH ROW EXECUTE FUNCTION assistant.guard_runtime_invocation();

-- Dispatch rechecks authority in the same transaction that consumes approval.
-- A recovery owner may observe an existing execution; it cannot redispatch a write.
CREATE FUNCTION assistant.guard_capability_dispatch_authority() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, assistant AS $$
BEGIN
    IF NEW.dispatch_fence IS NOT NULL AND NEW.dispatch_fence IS DISTINCT FROM OLD.dispatch_fence THEN
        PERFORM 1 FROM assistant_runs r JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
            JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
            LEFT JOIN assistant_runtime_execution_owners o ON o.run_id=r.run_id
            WHERE r.run_id=NEW.run_id AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id
            AND r.session_id=NEW.session_id AND r.status='running' AND l.status='active'
            AND l.expires_at>clock_timestamp() AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
            AND (o.run_id IS NULL OR o.lease_until>clock_timestamp())
            AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=s.snapshot_id)
            FOR SHARE OF r,l,s;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'RUNTIME_DISPATCH_AUTHORITY_REVOKED' USING ERRCODE='42501';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER capability_dispatch_authority BEFORE UPDATE ON assistant.assistant_capability_executions
    FOR EACH ROW EXECUTE FUNCTION assistant.guard_capability_dispatch_authority();

CREATE OR REPLACE FUNCTION assistant.dispatch_assistant_capability_execution(p_execution_id uuid, p_tenant_id character varying, p_user_id character varying, p_session_id character varying, p_dispatch_fence uuid, p_lease_ms bigint DEFAULT 30000) RETURNS TABLE(dispatch_fence uuid, claimed boolean)
    LANGUAGE plpgsql
    AS $$
DECLARE
    v_row assistant.assistant_capability_executions;
    v_approval assistant.assistant_tool_approvals%ROWTYPE;
BEGIN
    PERFORM 1 FROM assistant_runs r JOIN assistant_capability_executions e ON e.run_id=r.run_id
        WHERE e.execution_id=p_execution_id AND e.tenant_id=p_tenant_id
        AND e.user_id=p_user_id AND e.session_id=p_session_id FOR SHARE OF r;
    IF NOT FOUND THEN RAISE EXCEPTION 'ASSISTANT_CAPABILITY_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
    SELECT * INTO v_row
      FROM assistant_capability_executions
     WHERE execution_id = p_execution_id
       AND tenant_id = p_tenant_id
       AND user_id = p_user_id
       AND session_id = p_session_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_SCOPE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;
    IF v_row.status IN (
        'succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown'
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_TERMINAL_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    IF v_row.dispatch_fence IS NOT NULL THEN
        -- A read can be recovered safely after a worker process loss. A
        -- write/unknown dispatch can only be reconciled by its execution
        -- receipt and must never be blindly repeated.
        IF v_row.effect <> 'read' THEN
            IF v_row.dispatch_fence IS DISTINCT FROM p_dispatch_fence THEN
                RAISE EXCEPTION 'ASSISTANT_CAPABILITY_DISPATCH_FENCE_MISMATCH'
                    USING ERRCODE = '42501';
            END IF;
            RETURN QUERY SELECT v_row.dispatch_fence, FALSE;
            RETURN;
        END IF;
        IF v_row.worker_lease_until IS NOT NULL
           AND v_row.worker_lease_until > clock_timestamp()
        THEN
            RETURN QUERY SELECT v_row.dispatch_fence, FALSE;
            RETURN;
        END IF;
        UPDATE assistant_capability_executions
           SET dispatch_fence = p_dispatch_fence,
               worker_lease_until = clock_timestamp() + make_interval(secs => (LEAST(GREATEST(p_lease_ms, 1000), 120000)::double precision / 1000.0)),
               updated_at = clock_timestamp()
         WHERE execution_id = p_execution_id;
        RETURN QUERY SELECT p_dispatch_fence, TRUE;
        RETURN;
    END IF;

    IF v_row.effect IN ('write', 'unknown') THEN
        SELECT * INTO v_approval
          FROM assistant_tool_approvals
         WHERE approval_id = v_row.approval_id
           AND tenant_id = v_row.tenant_id
           AND user_id = v_row.user_id
           AND session_id = v_row.session_id
           AND run_id = v_row.run_id
           AND tool_call_id = v_row.tool_call_id
           AND tool_name = v_row.capability_id
           AND status = 'approved'
           AND expires_at > clock_timestamp()
         FOR UPDATE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'ASSISTANT_CAPABILITY_APPROVAL_REQUIRED'
                USING ERRCODE = '42501';
        END IF;
        IF v_approval.arguments IS DISTINCT FROM v_row.arguments THEN
            RAISE EXCEPTION 'ASSISTANT_CAPABILITY_APPROVAL_ARGUMENT_MISMATCH'
                USING ERRCODE = '42501';
        END IF;
        UPDATE assistant_tool_approvals
           SET status = 'consumed', updated_at = clock_timestamp()
         WHERE approval_id = v_row.approval_id
           AND status = 'approved';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'ASSISTANT_CAPABILITY_APPROVAL_REPLAYED'
                USING ERRCODE = '42501';
        END IF;
    END IF;

    UPDATE assistant_capability_executions
       SET dispatch_fence = p_dispatch_fence,
           dispatched_at = clock_timestamp(),
           approval_status = CASE
               WHEN effect = 'read' THEN approval_status ELSE 'consumed' END,
           status = 'dispatched',
           worker_lease_until = clock_timestamp() + make_interval(secs => (LEAST(GREATEST(p_lease_ms, 1000), 120000)::double precision / 1000.0)),
           updated_at = clock_timestamp()
     WHERE execution_id = p_execution_id;
    RETURN QUERY SELECT p_dispatch_fence, TRUE;
END;
$$;

-- Admission lease identity is ephemeral; the original run/call/attempt,
-- descriptor, arguments and approval remain the immutable execution identity.
CREATE OR REPLACE FUNCTION assistant.reserve_assistant_capability_execution(
    p_execution_id UUID,
    p_lease_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_run_id UUID,
    p_tool_call_id VARCHAR,
    p_attempt_id VARCHAR,
    p_capability_id VARCHAR,
    p_capability_revision BIGINT,
    p_arguments JSONB,
    p_arguments_sha256 CHAR(64),
    p_idempotency_key VARCHAR,
    p_effect VARCHAR,
    p_approval_policy VARCHAR,
    p_approval_id UUID,
    p_approval_status VARCHAR,
    p_events_url TEXT,
    p_resource_binding JSONB DEFAULT '{}'::jsonb
) RETURNS assistant.assistant_capability_executions AS $$
DECLARE
    v_row assistant.assistant_capability_executions;
BEGIN
    IF jsonb_typeof(p_resource_binding) <> 'object' THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_RESOURCE_BINDING_INVALID'
            USING ERRCODE = '22023';
    END IF;
    SELECT * INTO v_row
      FROM assistant_capability_executions
     WHERE (run_id, tool_call_id, attempt_id) =
           (p_run_id, p_tool_call_id, p_attempt_id)
        OR (
            tenant_id = p_tenant_id
            AND user_id = p_user_id
            AND session_id = p_session_id
            AND idempotency_key = p_idempotency_key
        )
     ORDER BY created_at
     LIMIT 1
     FOR UPDATE;

    IF FOUND THEN
        IF v_row.tenant_id IS DISTINCT FROM p_tenant_id
           OR v_row.user_id IS DISTINCT FROM p_user_id
           OR v_row.session_id IS DISTINCT FROM p_session_id
           OR v_row.run_id IS DISTINCT FROM p_run_id
           OR v_row.tool_call_id IS DISTINCT FROM p_tool_call_id
           OR v_row.attempt_id IS DISTINCT FROM p_attempt_id
           OR v_row.capability_id IS DISTINCT FROM p_capability_id
           OR v_row.capability_revision IS DISTINCT FROM p_capability_revision
           OR v_row.arguments_sha256 IS DISTINCT FROM p_arguments_sha256
           OR v_row.idempotency_key IS DISTINCT FROM p_idempotency_key
           OR v_row.effect IS DISTINCT FROM p_effect
           OR v_row.approval_policy IS DISTINCT FROM p_approval_policy
           OR v_row.approval_id IS DISTINCT FROM p_approval_id
           OR v_row.resource_binding IS DISTINCT FROM p_resource_binding
        THEN
            RAISE EXCEPTION 'ASSISTANT_CAPABILITY_IDEMPOTENCY_CONFLICT'
                USING ERRCODE = '23505';
        END IF;
        RETURN v_row;
    END IF;

    INSERT INTO assistant_capability_executions (
        execution_id, lease_id, tenant_id, user_id, session_id, run_id,
        tool_call_id, attempt_id, capability_id, capability_revision,
        arguments, arguments_sha256, idempotency_key, effect,
        approval_policy, approval_id, approval_status, status, events_url,
        resource_binding
    ) VALUES (
        p_execution_id, p_lease_id, p_tenant_id, p_user_id, p_session_id,
        p_run_id, p_tool_call_id, p_attempt_id, p_capability_id,
        p_capability_revision, p_arguments, p_arguments_sha256,
        p_idempotency_key, p_effect, p_approval_policy, p_approval_id,
        p_approval_status,
        CASE WHEN p_approval_status = 'pending'
            THEN 'awaiting_approval' ELSE 'published' END,
        p_events_url, p_resource_binding
    ) RETURNING * INTO v_row;
    RETURN v_row;
EXCEPTION WHEN unique_violation THEN
    SELECT * INTO v_row
      FROM assistant_capability_executions
     WHERE (run_id, tool_call_id, attempt_id) =
           (p_run_id, p_tool_call_id, p_attempt_id)
        OR (
            tenant_id = p_tenant_id
            AND user_id = p_user_id
            AND session_id = p_session_id
            AND idempotency_key = p_idempotency_key
        )
     ORDER BY created_at
     LIMIT 1;
    IF FOUND
       AND v_row.tenant_id = p_tenant_id
       AND v_row.user_id = p_user_id
       AND v_row.session_id = p_session_id
       AND v_row.run_id = p_run_id
       AND v_row.tool_call_id = p_tool_call_id
       AND v_row.attempt_id = p_attempt_id
       AND v_row.capability_id = p_capability_id
       AND v_row.capability_revision = p_capability_revision
       AND v_row.arguments_sha256 = p_arguments_sha256
       AND v_row.idempotency_key = p_idempotency_key
       AND v_row.effect = p_effect
       AND v_row.approval_policy = p_approval_policy
       AND v_row.approval_id IS NOT DISTINCT FROM p_approval_id
       AND v_row.resource_binding IS NOT DISTINCT FROM p_resource_binding
    THEN
        RETURN v_row;
    END IF;
    RAISE EXCEPTION 'ASSISTANT_CAPABILITY_IDEMPOTENCY_CONFLICT'
        USING ERRCODE = '23505';
END;
$$ LANGUAGE plpgsql;


CREATE FUNCTION assistant.reserve_assistant_runtime_model_call(
    p_call UUID,p_lease UUID,p_hash CHAR(64),p_input BIGINT,p_output BIGINT,p_cost BIGINT,p_owner UUID,p_fence BIGINT
) RETURNS VOID LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE r UUID;
BEGIN
    SELECT run_id INTO r FROM assistant_runtime_model_leases WHERE lease_id=p_lease;
    PERFORM assert_runtime_execution(r,p_owner,p_fence);
    PERFORM reserve_assistant_runtime_model_call(p_call,p_lease,p_hash,p_input,p_output,p_cost);
END $$;

CREATE FUNCTION assistant.guard_execution_owner_context() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF NEW.run_id<>OLD.run_id OR NEW.runtime_thread_id<>OLD.runtime_thread_id OR NEW.context<>OLD.context THEN
        RAISE EXCEPTION 'RUNTIME_EXECUTION_CONTEXT_IMMUTABLE' USING ERRCODE='55000';
    END IF;
    IF NEW.owner_id=OLD.owner_id THEN
        IF NEW.fence<>OLD.fence OR OLD.lease_until<=clock_timestamp() THEN
            RAISE EXCEPTION 'RUNTIME_EXECUTION_FENCE_LOST' USING ERRCODE='42501';
        END IF;
    ELSIF OLD.lease_until>clock_timestamp() OR NEW.fence<>OLD.fence+1 THEN
        RAISE EXCEPTION 'RUNTIME_EXECUTION_ALREADY_OWNED' USING ERRCODE='42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER runtime_owner_context BEFORE UPDATE ON assistant.assistant_runtime_execution_owners
    FOR EACH ROW EXECUTE FUNCTION assistant.guard_execution_owner_context();

-- Reuse the existing checked, scoped write functions rather than giving
-- service roles unrestricted ledger DML. Frozen baseline remains unchanged.
ALTER FUNCTION assistant.append_assistant_runtime_item(UUID,UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,VARCHAR,VARCHAR,VARCHAR,VARCHAR,JSONB,CHAR) SECURITY DEFINER;
ALTER FUNCTION assistant.append_assistant_runtime_item(UUID,UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,VARCHAR,VARCHAR,VARCHAR,VARCHAR,JSONB,CHAR) SET search_path=pg_catalog,assistant;
ALTER FUNCTION assistant.reserve_assistant_capability_execution(UUID,UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,VARCHAR,BIGINT,JSONB,CHAR,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,TEXT,JSONB) SECURITY DEFINER;
ALTER FUNCTION assistant.reserve_assistant_capability_execution(UUID,UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,VARCHAR,BIGINT,JSONB,CHAR,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,TEXT,JSONB) SET search_path=pg_catalog,assistant;
ALTER FUNCTION assistant.dispatch_assistant_capability_execution(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,BIGINT) SECURITY DEFINER;
ALTER FUNCTION assistant.dispatch_assistant_capability_execution(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,BIGINT) SET search_path=pg_catalog,assistant;
ALTER FUNCTION assistant.append_assistant_capability_event(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,JSONB,UUID) SECURITY DEFINER;
ALTER FUNCTION assistant.append_assistant_capability_event(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,VARCHAR,VARCHAR,JSONB,UUID) SET search_path=pg_catalog,assistant;
REVOKE ALL ON FUNCTION assistant.claim_runtime_execution(UUID,UUID,JSONB) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.assert_runtime_execution(UUID,UUID,BIGINT),assistant.assert_runtime_execution_fence(UUID,UUID,BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.reserve_assistant_runtime_model_call(UUID,UUID,CHAR,BIGINT,BIGINT,BIGINT,UUID,BIGINT) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.cancel_runtime_execution(UUID,UUID,VARCHAR,VARCHAR,VARCHAR) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.assert_worker_execution_owner(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,BIGINT,BOOLEAN) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.lock_runtime_revocation(),assistant.guard_runtime_invocation(),assistant.guard_execution_owner_context(),assistant.guard_capability_dispatch_authority() FROM PUBLIC;

DO $permissions$
DECLARE prefix TEXT;
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Runtime recovery migration requires the authority owner role';
    END IF;
    prefix:=left(current_user,length(current_user)-5);
    EXECUTE format('GRANT SELECT,UPDATE ON assistant.assistant_runtime_execution_owners TO %I',prefix||'runtime');
    EXECUTE format('GRANT SELECT ON assistant.assistant_runtime_execution_owners TO %I,%I',prefix||'gateway',prefix||'capability_worker');
    EXECUTE format('GRANT SELECT,INSERT,UPDATE ON assistant.assistant_runtime_invocations TO %I',prefix||'runtime');
    EXECUTE format('GRANT SELECT ON assistant.assistant_capability_executions TO %I',prefix||'runtime');
    EXECUTE format('GRANT SELECT,INSERT ON assistant.quizzes TO %I',prefix||'capability_worker');
    EXECUTE format('GRANT INSERT ON assistant.quiz_questions TO %I',prefix||'capability_worker');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.cancel_runtime_execution(UUID,UUID,VARCHAR,VARCHAR,VARCHAR) TO %I',prefix||'runtime');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.assert_worker_execution_owner(UUID,VARCHAR,VARCHAR,VARCHAR,UUID,BIGINT,BOOLEAN) TO %I',prefix||'capability_worker');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.claim_runtime_execution(UUID,UUID,JSONB),assistant.assert_runtime_execution(UUID,UUID,BIGINT),assistant.assert_runtime_execution_fence(UUID,UUID,BIGINT) TO %I',prefix||'runtime');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.reserve_assistant_runtime_model_call(UUID,UUID,CHAR,BIGINT,BIGINT,BIGINT,UUID,BIGINT) TO %I',prefix||'gateway');
END $permissions$;
