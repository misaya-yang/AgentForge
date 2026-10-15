-- Gateway owns only admission scope evidence and deletion delivery receipts.
-- No FK to service/session: deletion and adapter changes must not erase the outbox.
CREATE TABLE gateway.sdk_session_barriers (
    tenant_id varchar(255) NOT NULL CHECK (length(btrim(tenant_id)) > 0),
    user_id varchar(255) NOT NULL CHECK (length(btrim(user_id)) > 0),
    session_id varchar(255) NOT NULL CHECK (length(btrim(session_id)) > 0),
    deleting boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id,session_id)
);
CREATE TABLE gateway.sdk_session_scopes (
    tenant_id varchar(255) NOT NULL,
    user_id varchar(255) NOT NULL,
    session_id varchar(255) NOT NULL,
    service_id varchar(255) NOT NULL CHECK (length(btrim(service_id)) > 0),
    framework varchar(32) NOT NULL CHECK (framework IN ('openai_agents','claude_agent_sdk')),
    state varchar(16) NOT NULL DEFAULT 'active' CHECK (state IN ('active','pending','deleted')),
    attempts bigint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    deletion_requested_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id,session_id,service_id,framework),
    FOREIGN KEY (tenant_id,user_id,session_id) REFERENCES gateway.sdk_session_barriers(tenant_id,user_id,session_id),
    CHECK ((state='active') = (deletion_requested_at IS NULL)),
    CHECK ((state='deleted') = (deleted_at IS NOT NULL))
);
CREATE INDEX sdk_session_cleanup_pending ON gateway.sdk_session_scopes(next_attempt_at) WHERE state='pending';
COMMENT ON TABLE gateway.sdk_session_scopes IS 'Server-owned SDK admission scope and retryable deletion outbox. Never user-editable service metadata; no provider credentials, prompts or transcripts.';
REVOKE ALL ON gateway.sdk_session_barriers,gateway.sdk_session_scopes FROM PUBLIC;
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'SDK cleanup migration requires the authority owner role';
    END IF;
    EXECUTE format('REVOKE ALL ON gateway.sdk_session_barriers,gateway.sdk_session_scopes FROM %I', prefix || 'gateway');
    EXECUTE format('GRANT SELECT,INSERT,UPDATE ON gateway.sdk_session_barriers,gateway.sdk_session_scopes TO %I', prefix || 'gateway');
END
$permissions$;
