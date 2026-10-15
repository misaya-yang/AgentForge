-- 098 - Gateway-owned Local Node control plane.
--
-- No table here references an execution process. Pairing, grants, actions and
-- device receipts remain usable when that service is removed.

BEGIN;

CREATE TABLE IF NOT EXISTS local_node_pairing_challenges (
    challenge_id UUID PRIMARY KEY,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    user_code_sha256 CHAR(64) NOT NULL CHECK (user_code_sha256 ~ '^[0-9a-f]{64}$'),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_local_node_pairing_owner
    ON local_node_pairing_challenges(tenant_id,user_id,created_at DESC);

CREATE TABLE IF NOT EXISTS local_node_devices (
    device_id VARCHAR(255) PRIMARY KEY,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    display_name VARCHAR(160) NOT NULL,
    platform VARCHAR(32) NOT NULL,
    node_version VARCHAR(64) NOT NULL,
    protocol_version VARCHAR(64) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'online',
    capability_revision BIGINT NOT NULL DEFAULT 1 CHECK (capability_revision >= 1),
    capabilities JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(capabilities)='array'),
    permission_snapshot_digest VARCHAR(80) NOT NULL DEFAULT '',
    last_seen_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(device_id,tenant_id,user_id),
    CHECK(status IN ('online','offline','stale','revoked'))
);
CREATE INDEX IF NOT EXISTS idx_local_node_devices_owner
    ON local_node_devices(tenant_id,user_id,created_at DESC);

CREATE TABLE IF NOT EXISTS local_node_channels (
    channel_id VARCHAR(255) PRIMARY KEY,
    device_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    fingerprint VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'online',
    receipt_cursor BIGINT NOT NULL DEFAULT 0 CHECK (receipt_cursor >= 0),
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY(device_id,tenant_id,user_id)
      REFERENCES local_node_devices(device_id,tenant_id,user_id) ON DELETE RESTRICT,
    CHECK(status IN ('online','offline','revoked'))
);
CREATE INDEX IF NOT EXISTS idx_local_node_channels_owner
    ON local_node_channels(tenant_id,user_id,device_id);

CREATE TABLE IF NOT EXISTS local_node_grants (
    grant_id UUID PRIMARY KEY,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    device_id VARCHAR(255) NOT NULL,
    kind VARCHAR(32) NOT NULL CHECK (kind IN ('workspace','app','domain')),
    display_name VARCHAR(160) NOT NULL,
    capabilities JSONB NOT NULL CHECK (jsonb_typeof(capabilities)='array'),
    resource_ref VARCHAR(255),
    session_id VARCHAR(255),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status VARCHAR(16) NOT NULL DEFAULT 'active' CHECK (status IN ('active','revoked','expired')),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY(device_id,tenant_id,user_id)
      REFERENCES local_node_devices(device_id,tenant_id,user_id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS idx_local_node_grants_owner
    ON local_node_grants(tenant_id,user_id,device_id,status);

CREATE TABLE IF NOT EXISTS local_node_executions (
    execution_id UUID PRIMARY KEY,
    lease_id UUID NOT NULL,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    run_id UUID NOT NULL,
    tool_call_id VARCHAR(255) NOT NULL,
    attempt_id VARCHAR(255) NOT NULL,
    device_id VARCHAR(255) NOT NULL,
    channel_id VARCHAR(255) NOT NULL,
    grant_id UUID,
    grant_revision BIGINT,
    capability_id VARCHAR(255) NOT NULL,
    capability_revision BIGINT NOT NULL CHECK (capability_revision >= 1),
    operation VARCHAR(255) NOT NULL,
    arguments JSONB NOT NULL CHECK (jsonb_typeof(arguments)='object'),
    arguments_sha256 CHAR(64) NOT NULL CHECK (arguments_sha256 ~ '^[0-9a-f]{64}$'),
    idempotency_key VARCHAR(255) NOT NULL,
    effect VARCHAR(16) NOT NULL CHECK(effect IN ('read','write','unknown')),
    approval_id UUID,
    approval_status VARCHAR(16) NOT NULL DEFAULT 'not_required'
      CHECK(approval_status IN ('not_required','pending','approved','consumed','denied','expired')),
    resource_binding JSONB NOT NULL DEFAULT '{}'::jsonb CHECK(jsonb_typeof(resource_binding)='object'),
    status VARCHAR(32) NOT NULL DEFAULT 'published'
      CHECK(status IN ('published','awaiting_approval','dispatched','running','succeeded','failed','cancelled','timeout','side_effect_unknown')),
    dispatch_fence UUID,
    dispatched_at TIMESTAMPTZ,
    receipt_cursor BIGINT NOT NULL DEFAULT 0 CHECK(receipt_cursor >= 0),
    result_summary JSONB,
    terminal_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(tenant_id,user_id,session_id,idempotency_key),
    UNIQUE(run_id,tool_call_id,attempt_id),
    FOREIGN KEY(device_id,tenant_id,user_id) REFERENCES local_node_devices(device_id,tenant_id,user_id) ON DELETE RESTRICT,
    FOREIGN KEY(channel_id) REFERENCES local_node_channels(channel_id) ON DELETE RESTRICT,
    FOREIGN KEY(grant_id) REFERENCES local_node_grants(grant_id) ON DELETE RESTRICT,
    CHECK((effect='read' AND approval_id IS NULL AND approval_status='not_required') OR
          (effect IN ('write','unknown') AND approval_id IS NOT NULL AND approval_status <> 'not_required'))
);
CREATE INDEX IF NOT EXISTS idx_local_node_executions_recovery
    ON local_node_executions(status,updated_at)
    WHERE status IN ('published','awaiting_approval','dispatched','running');

CREATE TABLE IF NOT EXISTS local_node_receipts (
    execution_id UUID NOT NULL REFERENCES local_node_executions(execution_id) ON DELETE RESTRICT,
    event_id UUID NOT NULL,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    device_id VARCHAR(255) NOT NULL,
    channel_id VARCHAR(255) NOT NULL,
    dispatch_fence UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK(sequence >= 1),
    event VARCHAR(128) NOT NULL,
    status VARCHAR(32) NOT NULL CHECK(status IN ('running','succeeded','failed','cancelled','timeout','side_effect_unknown')),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb CHECK(jsonb_typeof(payload)='object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(execution_id,sequence),
    UNIQUE(execution_id,event_id)
);

CREATE TABLE IF NOT EXISTS local_node_events (
    event_id UUID PRIMARY KEY,
    device_sequence BIGSERIAL UNIQUE,
    execution_id UUID NOT NULL,
    tenant_id VARCHAR(128) NOT NULL,
    user_id VARCHAR(128) NOT NULL,
    device_id VARCHAR(255) NOT NULL,
    channel_id VARCHAR(255) NOT NULL,
    sequence BIGINT NOT NULL,
    event VARCHAR(128) NOT NULL,
    status VARCHAR(32) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb CHECK(jsonb_typeof(payload)='object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY(execution_id,sequence) REFERENCES local_node_receipts(execution_id,sequence) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS idx_local_node_events_owner_cursor
    ON local_node_events(tenant_id,user_id,device_id,device_sequence);

CREATE OR REPLACE FUNCTION claim_local_node_dispatch(
    p_execution_id UUID, p_tenant_id VARCHAR, p_user_id VARCHAR,
    p_session_id VARCHAR, p_fence UUID, p_effect VARCHAR
) RETURNS BOOLEAN AS $$
DECLARE v_row local_node_executions;
DECLARE v_grant_revision BIGINT;
DECLARE v_grant_status VARCHAR;
BEGIN
    SELECT * INTO v_row FROM local_node_executions
     WHERE execution_id=p_execution_id AND tenant_id=p_tenant_id AND user_id=p_user_id
       AND session_id=p_session_id FOR UPDATE;
    IF NOT FOUND OR v_row.dispatch_fence IS NOT NULL OR
       v_row.status IN ('succeeded','failed','cancelled','timeout','side_effect_unknown') THEN
        RETURN FALSE;
    END IF;
    IF p_effect IN ('write','unknown') AND v_row.approval_status <> 'approved' THEN
        RETURN FALSE;
    END IF;
    IF v_row.grant_id IS NOT NULL THEN
        SELECT revision,status INTO v_grant_revision,v_grant_status
          FROM local_node_grants WHERE grant_id=v_row.grant_id
            AND tenant_id=v_row.tenant_id AND user_id=v_row.user_id
            AND device_id=v_row.device_id;
        IF NOT FOUND OR v_grant_status <> 'active'
           OR v_grant_revision IS DISTINCT FROM v_row.grant_revision THEN
            RETURN FALSE;
        END IF;
    END IF;
    UPDATE local_node_executions SET dispatch_fence=p_fence,dispatched_at=now(),
      status='dispatched',approval_status=CASE WHEN p_effect IN ('write','unknown') THEN 'consumed' ELSE approval_status END,
      updated_at=now() WHERE execution_id=p_execution_id;
    RETURN TRUE;
END;
$$ LANGUAGE plpgsql;

COMMIT;
