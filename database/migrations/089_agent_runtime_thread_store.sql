-- 089 — Agent Harness runtime ownership, immutable snapshots, and append-only items.
--
-- Additive only. Existing sessions and Python AgentLoop runs remain unchanged.
-- The new tables are the durable Thread/Turn/Item authority for Agent-owned
-- sessions; sessions.history remains legacy import input and V1 compatibility.

BEGIN;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'sessions_runtime_owner_scope_key'
          AND conrelid = 'sessions'::regclass
    ) THEN
        ALTER TABLE sessions
            ADD CONSTRAINT sessions_runtime_owner_scope_key
            UNIQUE (tenant_id, user_id, session_id);
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS assistant_session_runtime_assignments (
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    runtime_owner VARCHAR(32) NOT NULL,
    kernel_revision VARCHAR(100),
    assignment_reason VARCHAR(64) NOT NULL DEFAULT 'default_policy',
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_session_runtime_assignments_pk
        PRIMARY KEY (tenant_id, user_id, session_id),
    CONSTRAINT assistant_session_runtime_assignments_owner_check
        CHECK (runtime_owner = 'agent_runtime'),
    CONSTRAINT assistant_session_runtime_assignments_kernel_check
        CHECK (
            (runtime_owner = 'agent_runtime' AND kernel_revision IS NOT NULL)
        ),
    CONSTRAINT assistant_session_runtime_assignments_session_fk
        FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES sessions(tenant_id, user_id, session_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_assistant_session_runtime_assignments_owner
    ON assistant_session_runtime_assignments(runtime_owner, assigned_at DESC);

CREATE OR REPLACE FUNCTION assistant_runtime_reject_assignment_update()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'ASSISTANT_RUNTIME_ASSIGNMENT_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS assistant_session_runtime_assignments_immutable
    ON assistant_session_runtime_assignments;
CREATE TRIGGER assistant_session_runtime_assignments_immutable
    BEFORE UPDATE ON assistant_session_runtime_assignments
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_assignment_update();

CREATE TABLE IF NOT EXISTS assistant_runtime_threads (
    runtime_thread_id UUID PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    kernel_owner VARCHAR(32) NOT NULL DEFAULT 'agent_runtime',
    owner_revision BIGINT NOT NULL DEFAULT 1 CHECK (owner_revision >= 1),
    source_kind VARCHAR(32) NOT NULL DEFAULT 'native',
    import_status VARCHAR(32) NOT NULL DEFAULT 'not_required',
    source_history_sha256 CHAR(64),
    source_history_count INTEGER NOT NULL DEFAULT 0 CHECK (source_history_count >= 0),
    import_error_code VARCHAR(100),
    last_sequence BIGINT NOT NULL DEFAULT 0 CHECK (last_sequence >= 0),
    archived_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    imported_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_threads_kernel_check
        CHECK (kernel_owner IN ('python', 'agent_runtime')),
    CONSTRAINT assistant_runtime_threads_source_check
        CHECK (source_kind IN ('native', 'legacy_import')),
    CONSTRAINT assistant_runtime_threads_import_check
        CHECK (import_status IN (
            'not_required', 'pending', 'importing', 'ready', 'failed'
        )),
    CONSTRAINT assistant_runtime_threads_history_hash_check
        CHECK (
            source_history_sha256 IS NULL
            OR source_history_sha256 ~ '^[0-9a-f]{64}$'
        ),
    CONSTRAINT assistant_runtime_threads_session_fk
        FOREIGN KEY (tenant_id, user_id, session_id)
        REFERENCES sessions(tenant_id, user_id, session_id)
        ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_threads_scope_key
        UNIQUE (tenant_id, user_id, session_id),
    CONSTRAINT assistant_runtime_threads_thread_scope_key
        UNIQUE (runtime_thread_id, tenant_id, user_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_threads_owner
    ON assistant_runtime_threads(tenant_id, kernel_owner, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_assistant_runtime_threads_import
    ON assistant_runtime_threads(tenant_id, import_status, updated_at DESC)
    WHERE import_status <> 'ready' AND import_status <> 'not_required';

CREATE TABLE IF NOT EXISTS assistant_runtime_thread_members (
    kernel_thread_id UUID PRIMARY KEY,
    runtime_thread_id UUID NOT NULL,
    kernel_session_id UUID NOT NULL,
    parent_kernel_thread_id UUID,
    forked_from_kernel_thread_id UUID,
    relation_kind VARCHAR(32) NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_thread_members_relation_check
        CHECK (relation_kind IN ('root', 'subagent', 'fork')),
    CONSTRAINT assistant_runtime_thread_members_root_check
        CHECK (
            relation_kind <> 'root'
            OR (
                kernel_thread_id = runtime_thread_id
                AND parent_kernel_thread_id IS NULL
                AND forked_from_kernel_thread_id IS NULL
            )
        ),
    CONSTRAINT assistant_runtime_thread_members_metadata_check
        CHECK (jsonb_typeof(metadata) = 'object'),
    CONSTRAINT assistant_runtime_thread_members_root_fk
        FOREIGN KEY (runtime_thread_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_threads(
            runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_thread_members_scope_key
        UNIQUE (
            kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id
        ),
    CONSTRAINT assistant_runtime_thread_members_tree_key
        UNIQUE (kernel_thread_id, runtime_thread_id),
    CONSTRAINT assistant_runtime_thread_members_parent_fk
        FOREIGN KEY (parent_kernel_thread_id, runtime_thread_id)
        REFERENCES assistant_runtime_thread_members(
            kernel_thread_id, runtime_thread_id
        ) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT assistant_runtime_thread_members_fork_fk
        FOREIGN KEY (forked_from_kernel_thread_id, runtime_thread_id)
        REFERENCES assistant_runtime_thread_members(
            kernel_thread_id, runtime_thread_id
        ) DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_thread_members_root
    ON assistant_runtime_thread_members(
        runtime_thread_id, relation_kind, created_at
    );

CREATE TABLE IF NOT EXISTS assistant_runtime_thread_projections (
    kernel_thread_id UUID PRIMARY KEY,
    runtime_thread_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    projection JSONB NOT NULL,
    archived_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_thread_projections_object_check
        CHECK (jsonb_typeof(projection) = 'object'),
    CONSTRAINT assistant_runtime_thread_projections_member_fk
        FOREIGN KEY (
            kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id
        ) REFERENCES assistant_runtime_thread_members(
            kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_thread_projections_root
    ON assistant_runtime_thread_projections(
        runtime_thread_id, archived_at, updated_at DESC
    );

CREATE TABLE IF NOT EXISTS assistant_runtime_snapshots (
    snapshot_id UUID PRIMARY KEY,
    runtime_thread_id UUID NOT NULL,
    run_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    schema_version VARCHAR(64) NOT NULL,
    agent_spec_hash CHAR(64),
    capability_revision BIGINT NOT NULL DEFAULT 1 CHECK (capability_revision >= 1),
    selected_reasoning_option VARCHAR(100),
    snapshot JSONB NOT NULL,
    snapshot_sha256 CHAR(64) NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_snapshots_hash_check
        CHECK (snapshot_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_runtime_snapshots_agent_hash_check
        CHECK (agent_spec_hash IS NULL OR agent_spec_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_runtime_snapshots_object_check
        CHECK (jsonb_typeof(snapshot) = 'object'),
    CONSTRAINT assistant_runtime_snapshots_thread_fk
        FOREIGN KEY (runtime_thread_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_threads(
            runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_snapshots_run_key UNIQUE (run_id),
    CONSTRAINT assistant_runtime_snapshots_scope_key
        UNIQUE (snapshot_id, tenant_id, user_id, session_id),
    CONSTRAINT assistant_runtime_snapshots_run_scope_key
        UNIQUE (snapshot_id, run_id, tenant_id, user_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_snapshots_thread
    ON assistant_runtime_snapshots(runtime_thread_id, created_at DESC);

CREATE TABLE IF NOT EXISTS assistant_runtime_snapshot_revocations (
    snapshot_id UUID PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    reason_code VARCHAR(100) NOT NULL,
    revoked_by VARCHAR(255) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_snapshot_revocations_metadata_check
        CHECK (jsonb_typeof(metadata) = 'object'),
    CONSTRAINT assistant_runtime_snapshot_revocations_snapshot_fk
        FOREIGN KEY (snapshot_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_snapshots(
            snapshot_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_snapshot_revocations_scope
    ON assistant_runtime_snapshot_revocations(
        tenant_id, user_id, session_id, revoked_at DESC
    );

CREATE TABLE IF NOT EXISTS assistant_runtime_items (
    runtime_thread_id UUID NOT NULL,
    kernel_thread_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence >= 1),
    event_id UUID NOT NULL,
    event_key VARCHAR(255) NOT NULL,
    turn_id VARCHAR(255),
    item_id VARCHAR(255),
    event_type VARCHAR(100) NOT NULL,
    item_type VARCHAR(100),
    status VARCHAR(64),
    payload JSONB NOT NULL,
    payload_sha256 CHAR(64) NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT assistant_runtime_items_pk PRIMARY KEY (runtime_thread_id, sequence),
    CONSTRAINT assistant_runtime_items_event_key UNIQUE (runtime_thread_id, event_key),
    CONSTRAINT assistant_runtime_items_event_id UNIQUE (runtime_thread_id, event_id),
    CONSTRAINT assistant_runtime_items_payload_hash_check
        CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT assistant_runtime_items_payload_object_check
        CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT assistant_runtime_items_thread_fk
        FOREIGN KEY (runtime_thread_id, tenant_id, user_id, session_id)
        REFERENCES assistant_runtime_threads(
            runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT,
    CONSTRAINT assistant_runtime_items_member_fk
        FOREIGN KEY (
            kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id
        ) REFERENCES assistant_runtime_thread_members(
            kernel_thread_id, runtime_thread_id, tenant_id, user_id, session_id
        ) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_assistant_runtime_items_turn
    ON assistant_runtime_items(runtime_thread_id, turn_id, sequence);
CREATE INDEX IF NOT EXISTS idx_assistant_runtime_items_item
    ON assistant_runtime_items(runtime_thread_id, item_id, sequence)
    WHERE item_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_assistant_runtime_items_scope_cursor
    ON assistant_runtime_items(tenant_id, user_id, session_id, sequence);
CREATE INDEX IF NOT EXISTS idx_assistant_runtime_items_member_cursor
    ON assistant_runtime_items(kernel_thread_id, sequence);

ALTER TABLE assistant_runs
    ADD COLUMN IF NOT EXISTS harness_thread_id UUID,
    ADD COLUMN IF NOT EXISTS harness_turn_id VARCHAR(255),
    ADD COLUMN IF NOT EXISTS runtime_snapshot_id UUID,
    ADD COLUMN IF NOT EXISTS kernel_revision VARCHAR(100),
    ADD COLUMN IF NOT EXISTS capability_revision BIGINT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_runs_harness_shape_check'
          AND conrelid = 'assistant_runs'::regclass
    ) THEN
        ALTER TABLE assistant_runs
            ADD CONSTRAINT assistant_runs_harness_shape_check CHECK (
                (engine <> 'agent_runtime'
                    AND harness_thread_id IS NULL
                    AND harness_turn_id IS NULL
                    AND runtime_snapshot_id IS NULL
                    AND kernel_revision IS NULL
                    AND capability_revision IS NULL)
                OR
                (engine = 'agent_runtime'
                    AND harness_thread_id IS NOT NULL
                    AND harness_turn_id IS NOT NULL
                    AND runtime_snapshot_id IS NOT NULL
                    AND kernel_revision IS NOT NULL
                    AND capability_revision >= 1)
            );
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_runs_harness_thread_fk'
          AND conrelid = 'assistant_runs'::regclass
    ) THEN
        ALTER TABLE assistant_runs
            ADD CONSTRAINT assistant_runs_harness_thread_fk
            FOREIGN KEY (harness_thread_id, tenant_id, user_id, session_id)
            REFERENCES assistant_runtime_threads(
                runtime_thread_id, tenant_id, user_id, session_id
            ) DEFERRABLE INITIALLY DEFERRED;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_runs_runtime_snapshot_fk'
          AND conrelid = 'assistant_runs'::regclass
    ) THEN
        ALTER TABLE assistant_runs
            ADD CONSTRAINT assistant_runs_runtime_snapshot_fk
            FOREIGN KEY (
                runtime_snapshot_id, run_id, tenant_id, user_id, session_id
            )
            REFERENCES assistant_runtime_snapshots(
                snapshot_id, run_id, tenant_id, user_id, session_id
            ) DEFERRABLE INITIALLY DEFERRED;
    END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_assistant_runs_harness_thread
    ON assistant_runs(tenant_id, harness_thread_id, created_at DESC)
    WHERE harness_thread_id IS NOT NULL;

CREATE OR REPLACE FUNCTION assistant_runtime_reject_immutable_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'ASSISTANT_RUNTIME_APPEND_ONLY'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS assistant_runtime_snapshots_immutable
    ON assistant_runtime_snapshots;
CREATE TRIGGER assistant_runtime_snapshots_immutable
    BEFORE UPDATE OR DELETE ON assistant_runtime_snapshots
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_immutable_mutation();

DROP TRIGGER IF EXISTS assistant_runtime_thread_members_immutable
    ON assistant_runtime_thread_members;
CREATE TRIGGER assistant_runtime_thread_members_immutable
    BEFORE UPDATE OR DELETE ON assistant_runtime_thread_members
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_immutable_mutation();

DROP TRIGGER IF EXISTS assistant_runtime_snapshot_revocations_immutable
    ON assistant_runtime_snapshot_revocations;
CREATE TRIGGER assistant_runtime_snapshot_revocations_immutable
    BEFORE UPDATE OR DELETE ON assistant_runtime_snapshot_revocations
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_immutable_mutation();

DROP TRIGGER IF EXISTS assistant_runtime_items_immutable
    ON assistant_runtime_items;
CREATE TRIGGER assistant_runtime_items_immutable
    BEFORE UPDATE OR DELETE ON assistant_runtime_items
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_immutable_mutation();

CREATE OR REPLACE FUNCTION append_assistant_runtime_item(
    p_runtime_thread_id UUID,
    p_kernel_thread_id UUID,
    p_tenant_id VARCHAR,
    p_user_id VARCHAR,
    p_session_id VARCHAR,
    p_event_id UUID,
    p_event_key VARCHAR,
    p_turn_id VARCHAR,
    p_item_id VARCHAR,
    p_event_type VARCHAR,
    p_item_type VARCHAR,
    p_status VARCHAR,
    p_payload JSONB,
    p_payload_sha256 CHAR(64)
)
RETURNS BIGINT AS $$
DECLARE
    existing_sequence BIGINT;
    next_sequence BIGINT;
BEGIN
    PERFORM 1
    FROM assistant_runtime_threads
    WHERE runtime_thread_id = p_runtime_thread_id
      AND tenant_id = p_tenant_id
      AND user_id = p_user_id
      AND session_id = p_session_id
      AND kernel_owner = 'agent_runtime'
      AND archived_at IS NULL
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM assistant_runtime_thread_members
        WHERE kernel_thread_id = p_kernel_thread_id
          AND runtime_thread_id = p_runtime_thread_id
          AND tenant_id = p_tenant_id
          AND user_id = p_user_id
          AND session_id = p_session_id
    ) THEN
        RAISE EXCEPTION 'ASSISTANT_RUNTIME_MEMBER_SCOPE_MISMATCH'
            USING ERRCODE = '42501';
    END IF;

    SELECT sequence INTO existing_sequence
    FROM assistant_runtime_items
    WHERE runtime_thread_id = p_runtime_thread_id
      AND event_key = p_event_key;

    IF existing_sequence IS NOT NULL THEN
        IF NOT EXISTS (
            SELECT 1 FROM assistant_runtime_items
            WHERE runtime_thread_id = p_runtime_thread_id
              AND event_key = p_event_key
              AND kernel_thread_id = p_kernel_thread_id
              AND event_id = p_event_id
              AND payload_sha256 = p_payload_sha256
        ) THEN
            RAISE EXCEPTION 'ASSISTANT_RUNTIME_EVENT_KEY_CONFLICT'
                USING ERRCODE = '23505';
        END IF;
        RETURN existing_sequence;
    END IF;

    UPDATE assistant_runtime_threads
    SET last_sequence = last_sequence + 1,
        updated_at = NOW()
    WHERE runtime_thread_id = p_runtime_thread_id
    RETURNING last_sequence INTO next_sequence;

    INSERT INTO assistant_runtime_items (
        runtime_thread_id, kernel_thread_id, sequence,
        event_id, event_key, turn_id, item_id,
        event_type, item_type, status, payload, payload_sha256,
        tenant_id, user_id, session_id
    ) VALUES (
        p_runtime_thread_id, p_kernel_thread_id, next_sequence,
        p_event_id, p_event_key,
        p_turn_id, p_item_id, p_event_type, p_item_type, p_status,
        p_payload, p_payload_sha256, p_tenant_id, p_user_id, p_session_id
    );

    RETURN next_sequence;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_assistant_runtime_threads_timestamp
    ON assistant_runtime_threads;
CREATE TRIGGER update_assistant_runtime_threads_timestamp
    BEFORE UPDATE ON assistant_runtime_threads
    FOR EACH ROW EXECUTE FUNCTION update_assistant_gateway_timestamp();

DROP TRIGGER IF EXISTS update_assistant_runtime_thread_projections_timestamp
    ON assistant_runtime_thread_projections;
CREATE TRIGGER update_assistant_runtime_thread_projections_timestamp
    BEFORE UPDATE ON assistant_runtime_thread_projections
    FOR EACH ROW EXECUTE FUNCTION update_assistant_gateway_timestamp();

COMMENT ON TABLE assistant_runtime_threads IS
    'One durable Agent Thread owner per tenant/user/session; legacy import metadata is bounded and hash-addressed.';
COMMENT ON TABLE assistant_session_runtime_assignments IS
    'Immutable per-session selection of the Python control or Agent candidate runtime; assignment never depends on prompt content.';
COMMENT ON TABLE assistant_runtime_thread_members IS
    'Immutable root, subagent, and fork membership for every Agent Thread in one platform session.';
COMMENT ON TABLE assistant_runtime_thread_projections IS
    'Mutable discovery projection for immutable Agent member identity and append-only history.';
COMMENT ON TABLE assistant_runtime_snapshots IS
    'Immutable, secret-free Agent runtime snapshots. Credentials are resolved outside this JSON document.';
COMMENT ON TABLE assistant_runtime_snapshot_revocations IS
    'Append-only revocation receipts for immutable runtime snapshots.';
COMMENT ON TABLE assistant_runtime_items IS
    'Append-only Agent Thread/Turn/Item event log with per-thread monotonic sequence and idempotent event keys.';

COMMIT;
