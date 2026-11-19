-- 090 — Secret-free Agent runtime snapshots and bounded model-plane leases.
--
-- Additive only. The Gateway is the sole issuer and consumer of these leases;
-- provider credentials never enter a snapshot, lease, Runtime request, or
-- Agent model context.

BEGIN;

CREATE TABLE IF NOT EXISTS assistant_runtime_model_leases (
    lease_id UUID PRIMARY KEY,
    snapshot_id UUID NOT NULL,
    run_id UUID NOT NULL,
    runtime_thread_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    schema_version VARCHAR(64) NOT NULL,
    provider_id VARCHAR(255) NOT NULL,
    model_id VARCHAR(255) NOT NULL,
    provider_revision VARCHAR(128) NOT NULL,
    capability_revision BIGINT NOT NULL CHECK (capability_revision >= 1),
    nonce_sha256 CHAR(64) NOT NULL,
    max_calls INTEGER NOT NULL CHECK (max_calls BETWEEN 1 AND 128),
    calls_reserved INTEGER NOT NULL DEFAULT 0 CHECK (calls_reserved >= 0),
    calls_completed INTEGER NOT NULL DEFAULT 0 CHECK (calls_completed >= 0),
    max_input_tokens BIGINT NOT NULL CHECK (max_input_tokens > 0),
    max_output_tokens BIGINT NOT NULL CHECK (max_output_tokens > 0),
    reserved_input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (reserved_input_tokens >= 0),
    reserved_output_tokens BIGINT NOT NULL DEFAULT 0 CHECK (reserved_output_tokens >= 0),
    used_input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (used_input_tokens >= 0),
    used_output_tokens BIGINT NOT NULL DEFAULT 0 CHECK (used_output_tokens >= 0),
    max_cost_microusd BIGINT NOT NULL CHECK (max_cost_microusd > 0),
    reserved_cost_microusd BIGINT NOT NULL DEFAULT 0 CHECK (reserved_cost_microusd >= 0),
    used_cost_microusd BIGINT NOT NULL DEFAULT 0 CHECK (used_cost_microusd >= 0),
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    issued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    revoked_reason VARCHAR(100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_model_leases_status_check
        CHECK (status IN ('active', 'exhausted', 'revoked', 'expired')),
    CONSTRAINT assistant_runtime_model_leases_nonce_check
        CHECK (nonce_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_runtime_model_leases_time_check
        CHECK (expires_at > issued_at),
    CONSTRAINT assistant_runtime_model_leases_counter_check
        CHECK (calls_completed <= calls_reserved AND calls_reserved <= max_calls),
    CONSTRAINT assistant_runtime_model_leases_snapshot_fk
        FOREIGN KEY (snapshot_id, run_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_snapshots(
            snapshot_id, run_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_model_leases_run_fk
        FOREIGN KEY (run_id)
        REFERENCES assistant_runs(run_id) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_model_leases_thread_fk
        FOREIGN KEY (runtime_thread_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_threads(
            runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_model_leases_snapshot_key UNIQUE (snapshot_id),
    CONSTRAINT assistant_runtime_model_leases_scope_key
        UNIQUE (lease_id, snapshot_id, run_id, tenant_id, user_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_model_leases_active
    ON assistant_runtime_model_leases(tenant_id, status, expires_at)
    WHERE status = 'active';

CREATE TABLE IF NOT EXISTS assistant_runtime_model_calls (
    call_id UUID PRIMARY KEY,
    lease_id UUID NOT NULL,
    snapshot_id UUID NOT NULL,
    run_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    request_sha256 CHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'reserved',
    estimated_input_tokens BIGINT NOT NULL CHECK (estimated_input_tokens >= 0),
    reserved_output_tokens BIGINT NOT NULL CHECK (reserved_output_tokens > 0),
    reserved_cost_microusd BIGINT NOT NULL CHECK (reserved_cost_microusd >= 0),
    input_tokens BIGINT CHECK (input_tokens IS NULL OR input_tokens >= 0),
    output_tokens BIGINT CHECK (output_tokens IS NULL OR output_tokens >= 0),
    cost_microusd BIGINT CHECK (cost_microusd IS NULL OR cost_microusd >= 0),
    provider_request_id VARCHAR(255),
    error_code VARCHAR(100),
    reserved_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    dispatched_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_model_calls_hash_check
        CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_runtime_model_calls_status_check
        CHECK (status IN ('reserved', 'dispatched', 'completed', 'failed', 'unknown')),
    CONSTRAINT assistant_runtime_model_calls_lease_fk
        FOREIGN KEY (
            lease_id, snapshot_id, run_id, tenant_id, user_id, session_id
        ) REFERENCES assistant_runtime_model_leases(
            lease_id, snapshot_id, run_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_model_calls_request_key
        UNIQUE (lease_id, request_sha256)
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_model_calls_run
    ON assistant_runtime_model_calls(run_id, reserved_at);

CREATE OR REPLACE FUNCTION issue_assistant_runtime_turn(
    p_snapshot_id UUID,
    p_lease_id UUID,
    p_run_id UUID,
    p_runtime_thread_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_kernel_revision VARCHAR,
    p_snapshot_schema_version VARCHAR,
    p_snapshot JSONB,
    p_snapshot_sha256 CHAR(64),
    p_capability_revision BIGINT,
    p_selected_reasoning_option VARCHAR,
    p_lease_schema_version VARCHAR,
    p_provider_id VARCHAR,
    p_model_id VARCHAR,
    p_provider_revision VARCHAR,
    p_nonce_sha256 CHAR(64),
    p_max_calls INTEGER,
    p_max_input_tokens BIGINT,
    p_max_output_tokens BIGINT,
    p_max_cost_microusd BIGINT,
    p_expires_at TIMESTAMPTZ,
    p_request_preview TEXT
) RETURNS VOID AS $$
DECLARE
    v_owner VARCHAR(32);
    v_kernel_revision VARCHAR(100);
BEGIN
    SELECT runtime_owner, kernel_revision
      INTO v_owner, v_kernel_revision
      FROM assistant_session_runtime_assignments
     WHERE tenant_id = p_tenant_id
       AND user_id = p_user_id
       AND session_id = p_session_id
     FOR SHARE;
    IF v_owner IS DISTINCT FROM 'agent_runtime'
       OR v_kernel_revision IS DISTINCT FROM p_kernel_revision THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_ASSIGNMENT_MISMATCH'
            USING ERRCODE = '42501';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM assistant_runtime_threads
         WHERE runtime_thread_id = p_runtime_thread_id
           AND tenant_id = p_tenant_id
           AND user_id = p_user_id
           AND session_id = p_session_id
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;

    INSERT INTO assistant_runtime_snapshots (
        snapshot_id, runtime_thread_id, run_id, tenant_id, user_id,
        session_id, schema_version, capability_revision,
        selected_reasoning_option, snapshot, snapshot_sha256, expires_at
    ) VALUES (
        p_snapshot_id, p_runtime_thread_id, p_run_id, p_tenant_id, p_user_id,
        p_session_id, p_snapshot_schema_version, p_capability_revision,
        p_selected_reasoning_option, p_snapshot, p_snapshot_sha256, p_expires_at
    );

    INSERT INTO assistant_runs (
        run_id, tenant_id, user_id, session_id, status, engine,
        execution_profile, memory_mode, os_agent_enabled, request_preview,
        harness_thread_id, harness_turn_id, runtime_snapshot_id,
        kernel_revision, capability_revision
    ) VALUES (
        p_run_id, p_tenant_id, p_user_id, p_session_id, 'running',
        'agent_runtime', 'safe', 'off', FALSE, LEFT(p_request_preview, 500),
        p_runtime_thread_id, p_run_id::text, p_snapshot_id,
        p_kernel_revision, p_capability_revision
    );

    INSERT INTO assistant_runtime_model_leases (
        lease_id, snapshot_id, run_id, runtime_thread_id, tenant_id,
        user_id, session_id, schema_version, provider_id, model_id,
        provider_revision, capability_revision, nonce_sha256, max_calls,
        max_input_tokens, max_output_tokens, max_cost_microusd, expires_at
    ) VALUES (
        p_lease_id, p_snapshot_id, p_run_id, p_runtime_thread_id, p_tenant_id,
        p_user_id, p_session_id, p_lease_schema_version, p_provider_id,
        p_model_id, p_provider_revision, p_capability_revision,
        p_nonce_sha256, p_max_calls, p_max_input_tokens, p_max_output_tokens,
        p_max_cost_microusd, p_expires_at
    );
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION reserve_assistant_runtime_model_call(
    p_call_id UUID,
    p_lease_id UUID,
    p_request_sha256 CHAR(64),
    p_estimated_input_tokens BIGINT,
    p_reserved_output_tokens BIGINT,
    p_reserved_cost_microusd BIGINT
) RETURNS VOID AS $$
DECLARE
    v_lease assistant_runtime_model_leases%ROWTYPE;
BEGIN
    SELECT * INTO v_lease
      FROM assistant_runtime_model_leases
     WHERE lease_id = p_lease_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_LEASE_NOT_FOUND'
            USING ERRCODE = '42501';
    END IF;
    IF v_lease.status <> 'active' OR v_lease.expires_at <= NOW() THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_LEASE_INACTIVE'
            USING ERRCODE = '42501';
    END IF;
    IF EXISTS (
        SELECT 1 FROM assistant_runtime_snapshot_revocations
         WHERE snapshot_id = v_lease.snapshot_id
    ) OR NOT EXISTS (
        SELECT 1 FROM assistant_runs
         WHERE run_id = v_lease.run_id
           AND status = 'running'
           AND engine = 'agent_runtime'
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_LEASE_REVOKED'
            USING ERRCODE = '42501';
    END IF;
    IF v_lease.calls_reserved >= v_lease.max_calls
       OR v_lease.reserved_input_tokens + p_estimated_input_tokens > v_lease.max_input_tokens
       OR v_lease.reserved_output_tokens + p_reserved_output_tokens > v_lease.max_output_tokens
       OR v_lease.reserved_cost_microusd + p_reserved_cost_microusd > v_lease.max_cost_microusd THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_LEASE_BUDGET_EXHAUSTED'
            USING ERRCODE = '53000';
    END IF;
    IF EXISTS (
        SELECT 1 FROM assistant_runtime_model_calls
         WHERE lease_id = p_lease_id AND request_sha256 = p_request_sha256
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_MODEL_CALL_REPLAYED'
            USING ERRCODE = '55000';
    END IF;

    INSERT INTO assistant_runtime_model_calls (
        call_id, lease_id, snapshot_id, run_id, tenant_id, user_id,
        session_id, request_sha256, estimated_input_tokens,
        reserved_output_tokens, reserved_cost_microusd
    ) VALUES (
        p_call_id, v_lease.lease_id, v_lease.snapshot_id, v_lease.run_id,
        v_lease.tenant_id, v_lease.user_id, v_lease.session_id,
        p_request_sha256, p_estimated_input_tokens, p_reserved_output_tokens,
        p_reserved_cost_microusd
    );

    UPDATE assistant_runtime_model_leases
       SET calls_reserved = calls_reserved + 1,
           reserved_input_tokens = reserved_input_tokens + p_estimated_input_tokens,
           reserved_output_tokens = reserved_output_tokens + p_reserved_output_tokens,
           reserved_cost_microusd = reserved_cost_microusd + p_reserved_cost_microusd,
           updated_at = NOW()
     WHERE lease_id = p_lease_id;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION complete_assistant_runtime_model_call(
    p_call_id UUID,
    p_input_tokens BIGINT,
    p_output_tokens BIGINT,
    p_cost_microusd BIGINT,
    p_provider_request_id VARCHAR DEFAULT NULL
) RETURNS VOID AS $$
DECLARE
    v_lease_id UUID;
BEGIN
    UPDATE assistant_runtime_model_calls
       SET status = 'completed', input_tokens = p_input_tokens,
           output_tokens = p_output_tokens, cost_microusd = p_cost_microusd,
           provider_request_id = p_provider_request_id,
           completed_at = NOW(), updated_at = NOW()
     WHERE call_id = p_call_id AND status = 'dispatched'
     RETURNING lease_id INTO v_lease_id;
    IF v_lease_id IS NULL THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_MODEL_CALL_NOT_DISPATCHED'
            USING ERRCODE = '55000';
    END IF;
    UPDATE assistant_runtime_model_leases
       SET calls_completed = calls_completed + 1,
           used_input_tokens = used_input_tokens + p_input_tokens,
           used_output_tokens = used_output_tokens + p_output_tokens,
           used_cost_microusd = used_cost_microusd + p_cost_microusd,
           updated_at = NOW()
     WHERE lease_id = v_lease_id;
END;
$$ LANGUAGE plpgsql;

COMMIT;
