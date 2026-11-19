-- 096 - Durable Capability Contract V2 execution authority.
--
-- Additive only. Runtime publishes a scope-bound execution before dispatch;
-- the worker appends immutable events and advances the fenced projection.

BEGIN;

-- Bind approvals to one published tool call. Existing rows remain NULL and
-- are reconciled by the Runtime restart path; all new Runtime approvals set
-- this value before a capability lease can be issued.
ALTER TABLE assistant_tool_approvals
    ADD COLUMN IF NOT EXISTS tool_call_id VARCHAR(160);
ALTER TABLE assistant_tool_approvals
    ALTER COLUMN tool_name TYPE VARCHAR(160);

-- Per-service schema moves can materialize a Gateway-owned assistant_runs
-- projection after migration 073. Reassert the composite scope key on the
-- table visible to this migration before using it as a tenant-bound FK.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'assistant_runs_capability_scope_key'
          AND conrelid = 'assistant_runs'::regclass
    ) THEN
        ALTER TABLE assistant_runs
            ADD CONSTRAINT assistant_runs_capability_scope_key
            UNIQUE (run_id, tenant_id, user_id, session_id);
    END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_assistant_tool_approvals_run_call
    ON assistant_tool_approvals(run_id, tool_call_id)
    WHERE tool_call_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS assistant_capability_executions (
    execution_id UUID PRIMARY KEY,
    lease_id UUID NOT NULL,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    run_id UUID NOT NULL,
    tool_call_id VARCHAR(160) NOT NULL,
    attempt_id VARCHAR(160) NOT NULL,
    capability_id VARCHAR(160) NOT NULL,
    capability_revision BIGINT NOT NULL CHECK (capability_revision >= 1),
    arguments JSONB NOT NULL,
    arguments_sha256 CHAR(64) NOT NULL,
    idempotency_key VARCHAR(160) NOT NULL,
    effect VARCHAR(16) NOT NULL,
    approval_policy VARCHAR(16) NOT NULL,
    approval_id UUID,
    approval_status VARCHAR(20) NOT NULL DEFAULT 'not_required',
    -- Materialized, server-derived Runtime snapshot binding.  Arguments are
    -- untrusted input and are never used as the authority for resources.
    resource_binding JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'published',
    events_url TEXT NOT NULL,
    dispatch_fence UUID,
    dispatched_at TIMESTAMPTZ,
    result_summary JSONB,
    error_code VARCHAR(100),
    terminal_at TIMESTAMPTZ,
    last_event_sequence BIGINT NOT NULL DEFAULT 0 CHECK (last_event_sequence >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_capability_executions_call_key
        UNIQUE (run_id, tool_call_id, attempt_id),
    CONSTRAINT assistant_capability_executions_idempotency_key
        UNIQUE (tenant_id, user_id, session_id, idempotency_key),
    CONSTRAINT assistant_capability_executions_scope_key
        UNIQUE (
            execution_id, tenant_id, user_id, session_id,
            run_id, tool_call_id, attempt_id
        ),
    CONSTRAINT assistant_capability_executions_arguments_object_check
        CHECK (jsonb_typeof(arguments) = 'object'),
    CONSTRAINT assistant_capability_executions_resource_binding_object_check
        CHECK (jsonb_typeof(resource_binding) = 'object'),
    CONSTRAINT assistant_capability_executions_hash_check
        CHECK (arguments_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_capability_executions_effect_check
        CHECK (effect IN ('read', 'write', 'unknown')),
    CONSTRAINT assistant_capability_executions_approval_policy_check
        CHECK (approval_policy IN ('never', 'on_request', 'always')),
    CONSTRAINT assistant_capability_executions_approval_status_check
        CHECK (approval_status IN (
            'not_required', 'pending', 'approved', 'consumed', 'denied', 'expired'
        )),
    CONSTRAINT assistant_capability_executions_status_check
        CHECK (status IN (
            'published', 'awaiting_approval', 'dispatched', 'running',
            'succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown'
        )),
    CONSTRAINT assistant_capability_executions_approval_shape_check CHECK (
        (effect = 'read' AND approval_id IS NULL AND approval_status = 'not_required')
        OR
        (effect IN ('write', 'unknown') AND approval_id IS NOT NULL
            AND approval_status <> 'not_required')
    ),
    CONSTRAINT assistant_capability_executions_dispatch_shape_check CHECK (
        (dispatch_fence IS NULL AND dispatched_at IS NULL)
        OR (dispatch_fence IS NOT NULL AND dispatched_at IS NOT NULL)
    ),
    CONSTRAINT assistant_capability_executions_terminal_shape_check CHECK (
        (status IN ('succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown')
            AND terminal_at IS NOT NULL)
        OR
        (status NOT IN ('succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown')
            AND terminal_at IS NULL)
    ),
    CONSTRAINT assistant_capability_executions_events_url_check
        CHECK (events_url ~ '^/internal/v2/capabilities/executions/[0-9a-f-]+/events$'),
    CONSTRAINT assistant_capability_executions_run_scope_fk
        FOREIGN KEY (run_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runs(run_id, tenant_id, user_id, session_id)
        ON DELETE RESTRICT,
    CONSTRAINT assistant_capability_executions_approval_fk
        FOREIGN KEY (approval_id)
        REFERENCES assistant_tool_approvals(approval_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_assistant_capability_executions_scope
    ON assistant_capability_executions(
        tenant_id, user_id, session_id, created_at DESC
    );

ALTER TABLE assistant_capability_executions
    ADD COLUMN IF NOT EXISTS resource_binding JSONB NOT NULL DEFAULT '{}'::jsonb;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'assistant_capability_executions_resource_binding_object_check'
           AND conrelid = 'assistant_capability_executions'::regclass
    ) THEN
        ALTER TABLE assistant_capability_executions
            ADD CONSTRAINT assistant_capability_executions_resource_binding_object_check
            CHECK (jsonb_typeof(resource_binding) = 'object');
    END IF;
END;
$$;

-- The dispatch fence is also the worker claim token. A bounded lease lets a
-- fresh worker recover a read after process loss without letting two workers
-- execute the same read concurrently. Writes are never reclaimed.
ALTER TABLE assistant_capability_executions
    ADD COLUMN IF NOT EXISTS worker_lease_until TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_assistant_capability_executions_active
    ON assistant_capability_executions(status, updated_at)
    WHERE status IN ('published', 'awaiting_approval', 'dispatched', 'running');

CREATE TABLE IF NOT EXISTS assistant_capability_events (
    execution_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence >= 1),
    event_id UUID NOT NULL,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    run_id UUID NOT NULL,
    tool_call_id VARCHAR(160) NOT NULL,
    attempt_id VARCHAR(160) NOT NULL,
    event VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (execution_id, sequence),
    UNIQUE (execution_id, event_id),
    CONSTRAINT assistant_capability_events_payload_check
        CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT assistant_capability_events_status_check
        CHECK (status IN (
            'published', 'awaiting_approval', 'dispatched', 'running',
            'succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown'
        )),
    CONSTRAINT assistant_capability_events_execution_fk
        FOREIGN KEY (
            execution_id, tenant_id, user_id, session_id,
            run_id, tool_call_id, attempt_id
        ) REFERENCES assistant_capability_executions(
            execution_id, tenant_id, user_id, session_id,
            run_id, tool_call_id, attempt_id
        ) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_assistant_capability_events_scope_cursor
    ON assistant_capability_events(tenant_id, execution_id, sequence);

CREATE OR REPLACE FUNCTION assistant_capability_reject_terminal_mutation()
RETURNS TRIGGER AS $$
BEGIN
    IF OLD.status IN (
        'succeeded', 'failed', 'cancelled', 'timeout', 'side_effect_unknown'
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_TERMINAL_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS assistant_capability_terminal_immutable
    ON assistant_capability_executions;
CREATE TRIGGER assistant_capability_terminal_immutable
    BEFORE UPDATE ON assistant_capability_executions
    FOR EACH ROW EXECUTE FUNCTION assistant_capability_reject_terminal_mutation();

CREATE OR REPLACE FUNCTION assistant_capability_reject_event_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'ASSISTANT_CAPABILITY_EVENT_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS assistant_capability_event_immutable
    ON assistant_capability_events;
CREATE TRIGGER assistant_capability_event_immutable
    BEFORE UPDATE OR DELETE ON assistant_capability_events
    FOR EACH ROW EXECUTE FUNCTION assistant_capability_reject_event_mutation();

DROP FUNCTION IF EXISTS reserve_assistant_capability_execution(
    UUID, UUID, VARCHAR, VARCHAR, VARCHAR, UUID, VARCHAR, VARCHAR,
    VARCHAR, BIGINT, JSONB, CHAR, VARCHAR, VARCHAR, VARCHAR, UUID,
    VARCHAR, TEXT
);
DROP FUNCTION IF EXISTS reserve_assistant_capability_execution(
    UUID, UUID, VARCHAR, VARCHAR, VARCHAR, UUID, VARCHAR, VARCHAR,
    VARCHAR, BIGINT, JSONB, CHAR, VARCHAR, VARCHAR, VARCHAR, UUID,
    VARCHAR, TEXT, JSONB
);
CREATE FUNCTION reserve_assistant_capability_execution(
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
) RETURNS assistant_capability_executions AS $$
DECLARE
    v_row assistant_capability_executions;
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
        IF v_row.lease_id IS DISTINCT FROM p_lease_id
           OR v_row.tenant_id IS DISTINCT FROM p_tenant_id
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
       AND v_row.lease_id = p_lease_id
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

DROP FUNCTION IF EXISTS dispatch_assistant_capability_execution(
    UUID, VARCHAR, VARCHAR, VARCHAR, UUID
);
DROP FUNCTION IF EXISTS dispatch_assistant_capability_execution(
    UUID, VARCHAR, VARCHAR, VARCHAR, UUID, BIGINT
);
CREATE FUNCTION dispatch_assistant_capability_execution(
    p_execution_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_dispatch_fence UUID,
    p_lease_ms BIGINT DEFAULT 30000
) RETURNS TABLE (dispatch_fence UUID, claimed BOOLEAN) AS $$
DECLARE
    v_row assistant_capability_executions;
    v_approval assistant_tool_approvals%ROWTYPE;
BEGIN
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
           AND v_row.worker_lease_until > NOW()
        THEN
            RETURN QUERY SELECT v_row.dispatch_fence, FALSE;
            RETURN;
        END IF;
        UPDATE assistant_capability_executions
           SET dispatch_fence = p_dispatch_fence,
               worker_lease_until = NOW() + make_interval(secs => (LEAST(GREATEST(p_lease_ms, 1000), 120000)::double precision / 1000.0)),
               updated_at = NOW()
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
           AND expires_at > NOW()
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
           SET status = 'consumed', updated_at = NOW()
         WHERE approval_id = v_row.approval_id
           AND status = 'approved';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'ASSISTANT_CAPABILITY_APPROVAL_REPLAYED'
                USING ERRCODE = '42501';
        END IF;
    END IF;

    UPDATE assistant_capability_executions
       SET dispatch_fence = p_dispatch_fence,
           dispatched_at = NOW(),
           approval_status = CASE
               WHEN effect = 'read' THEN approval_status ELSE 'consumed' END,
           status = 'dispatched',
           worker_lease_until = NOW() + make_interval(secs => (LEAST(GREATEST(p_lease_ms, 1000), 120000)::double precision / 1000.0)),
           updated_at = NOW()
     WHERE execution_id = p_execution_id;
    RETURN QUERY SELECT p_dispatch_fence, TRUE;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION append_assistant_capability_event(
    p_execution_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_event_id UUID,
    p_event VARCHAR,
    p_status VARCHAR,
    p_payload JSONB DEFAULT '{}'::jsonb,
    p_dispatch_fence UUID DEFAULT NULL
) RETURNS BIGINT AS $$
DECLARE
    v_row assistant_capability_executions;
    v_existing_sequence BIGINT;
    v_sequence BIGINT;
BEGIN
    SELECT sequence INTO v_existing_sequence
      FROM assistant_capability_events
     WHERE execution_id = p_execution_id
       AND event_id = p_event_id
       AND tenant_id = p_tenant_id
       AND user_id = p_user_id
       AND session_id = p_session_id;
    IF FOUND THEN
        RETURN v_existing_sequence;
    END IF;

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
    IF (p_status = 'published' AND v_row.last_event_sequence <> 0)
       OR (p_status = 'dispatched' AND v_row.status NOT IN (
           'published', 'dispatched', 'running'))
       OR (p_status = 'running' AND v_row.status NOT IN ('dispatched', 'running'))
    THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_EVENT_INVALID'
            USING ERRCODE = '22023';
    END IF;
    IF p_dispatch_fence IS NOT NULL
       AND v_row.dispatch_fence IS DISTINCT FROM p_dispatch_fence
    THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_DISPATCH_FENCE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;
    IF p_status IN ('dispatched', 'running') AND v_row.dispatch_fence IS NULL THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_DISPATCH_REQUIRED'
            USING ERRCODE = '42501';
    END IF;
    IF p_status IN ('succeeded', 'timeout', 'side_effect_unknown')
       AND v_row.dispatch_fence IS NULL
    THEN
        RAISE EXCEPTION 'ASSISTANT_CAPABILITY_DISPATCH_REQUIRED'
            USING ERRCODE = '42501';
    END IF;

    v_sequence := v_row.last_event_sequence + 1;
    INSERT INTO assistant_capability_events (
        execution_id, sequence, event_id, tenant_id, user_id, session_id,
        run_id, tool_call_id, attempt_id, event, status, payload
    ) VALUES (
        p_execution_id, v_sequence, p_event_id, v_row.tenant_id,
        v_row.user_id, v_row.session_id, v_row.run_id, v_row.tool_call_id,
        v_row.attempt_id, p_event, p_status, p_payload
    );

    UPDATE assistant_capability_executions
       SET last_event_sequence = v_sequence,
           status = p_status,
           result_summary = CASE
               WHEN p_status IN (
                   'succeeded', 'failed', 'cancelled', 'timeout',
                   'side_effect_unknown'
               ) THEN p_payload->'result'
               ELSE result_summary
           END,
           error_code = CASE
               WHEN p_status IN ('failed', 'cancelled', 'timeout', 'side_effect_unknown')
               THEN NULLIF(p_payload->>'error_code', '')
               ELSE error_code
           END,
           terminal_at = CASE
               WHEN p_status IN (
                   'succeeded', 'failed', 'cancelled', 'timeout',
                   'side_effect_unknown'
               ) THEN NOW()
               ELSE NULL
           END,
           worker_lease_until = CASE
               WHEN p_status = 'running' THEN NOW() + INTERVAL '30 seconds'
               WHEN p_status IN (
                   'succeeded', 'failed', 'cancelled', 'timeout',
                   'side_effect_unknown'
               ) THEN NULL
               ELSE worker_lease_until
           END,
           updated_at = NOW()
     WHERE execution_id = p_execution_id;
    RETURN v_sequence;
EXCEPTION WHEN unique_violation THEN
    SELECT sequence INTO v_existing_sequence
      FROM assistant_capability_events
     WHERE execution_id = p_execution_id
       AND event_id = p_event_id
       AND tenant_id = p_tenant_id
       AND user_id = p_user_id
       AND session_id = p_session_id;
    IF FOUND THEN
        RETURN v_existing_sequence;
    END IF;
    RAISE;
END;
$$ LANGUAGE plpgsql;

COMMENT ON TABLE assistant_capability_executions IS
    'Scope-bound Capability Contract V2 executions and dispatch fences.';
COMMENT ON TABLE assistant_capability_events IS
    'Immutable, monotonically sequenced capability progress and terminal receipts.';

COMMIT;
