-- AS-08: Agent Studio operations, governance, and retention contracts.
-- Forward-only and additive. Immutable Versions/Publications and historical
-- audit rows are retained; application rollback does not require a down migration.

BEGIN;

ALTER TABLE audit_logs
    ADD COLUMN IF NOT EXISTS agent_id UUID,
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS publication_id UUID,
    ADD COLUMN IF NOT EXISTS channel VARCHAR(16),
    ADD COLUMN IF NOT EXISTS redaction_state JSONB NOT NULL DEFAULT '{}'::jsonb;

UPDATE audit_logs
SET agent_id = resource_id::uuid
WHERE agent_id IS NULL
  AND event_type = 'agent_studio'
  AND resource_type = 'agent'
  AND resource_id ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'audit_logs_agent_channel_check'
          AND conrelid = 'audit_logs'::regclass
    ) THEN
        ALTER TABLE audit_logs
            ADD CONSTRAINT audit_logs_agent_channel_check
            CHECK (channel IS NULL OR channel IN ('preview', 'hosted', 'embed', 'api', 'builtin'));
    END IF;
END $$;

CREATE OR REPLACE FUNCTION agent_audit_redact_jsonb(value JSONB)
RETURNS JSONB AS $$
DECLARE
    item JSONB;
BEGIN
    IF value IS NULL THEN
        RETURN '{}'::jsonb;
    END IF;
    IF jsonb_typeof(value) = 'object' THEN
        SELECT COALESCE(
            jsonb_object_agg(
                entry.key,
                CASE
                    WHEN regexp_replace(lower(entry.key), '[^a-z0-9]', '', 'g')
                         ~ '(authorization|authentication|apikey|apitoken|accesstoken|bearertoken|clientsecret|connectionstring|cookie|credential|headers|password|privatekey|prompt|refreshtoken|secret|snapshot|tokenhash|tokenref)'
                    THEN '"[REDACTED]"'::jsonb
                    ELSE agent_audit_redact_jsonb(entry.value)
                END
            ),
            '{}'::jsonb
        ) INTO item
        FROM jsonb_each(value) AS entry;
        RETURN item;
    END IF;
    IF jsonb_typeof(value) = 'array' THEN
        SELECT COALESCE(
            jsonb_agg(agent_audit_redact_jsonb(entry.value)),
            '[]'::jsonb
        ) INTO item
        FROM jsonb_array_elements(value) AS entry;
        RETURN item;
    END IF;
    IF jsonb_typeof(value) = 'string'
       AND (value #>> '{}') ~* '(bearer[[:space:]]+[^[:space:]]+|sk-[a-z0-9_-]+|authorization=|password=|token=|secret=)' THEN
        RETURN '"[REDACTED]"'::jsonb;
    END IF;
    RETURN value;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

CREATE OR REPLACE FUNCTION agent_audit_dimension_projection()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.event_type = 'agent_studio' AND NEW.resource_type = 'agent' THEN
        IF NEW.agent_id IS NULL
           AND NEW.resource_id ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' THEN
            NEW.agent_id := NEW.resource_id::uuid;
        END IF;
        IF NEW.agent_version_id IS NULL
           AND COALESCE(NEW.request_summary->>'agent_version_id', '')
               ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' THEN
            NEW.agent_version_id := (NEW.request_summary->>'agent_version_id')::uuid;
        END IF;
        IF NEW.publication_id IS NULL
           AND COALESCE(NEW.request_summary->>'publication_id', '')
               ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' THEN
            NEW.publication_id := (NEW.request_summary->>'publication_id')::uuid;
        END IF;
        IF NEW.channel IS NULL
           AND NEW.request_summary->>'channel' IN ('preview', 'hosted', 'embed', 'api', 'builtin') THEN
            NEW.channel := NEW.request_summary->>'channel';
        END IF;
        NEW.request_summary := agent_audit_redact_jsonb(
            COALESCE(NEW.request_summary, '{}'::jsonb)
        );
        NEW.response_summary := agent_audit_redact_jsonb(
            COALESCE(NEW.response_summary, '{}'::jsonb)
        );
        NEW.redaction_state := COALESCE(NEW.redaction_state, '{}'::jsonb)
            || '{"sensitive_fields":"removed"}'::jsonb;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_audit_dimension_projection_trigger ON audit_logs;
CREATE TRIGGER agent_audit_dimension_projection_trigger
    BEFORE INSERT OR UPDATE OF event_type, resource_type, resource_id,
        request_summary, response_summary ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION agent_audit_dimension_projection();

UPDATE audit_logs
SET agent_version_id = (request_summary->>'agent_version_id')::uuid
WHERE agent_version_id IS NULL
  AND event_type = 'agent_studio'
  AND COALESCE(request_summary->>'agent_version_id', '')
      ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$';

UPDATE audit_logs
SET publication_id = (request_summary->>'publication_id')::uuid
WHERE publication_id IS NULL
  AND event_type = 'agent_studio'
  AND COALESCE(request_summary->>'publication_id', '')
      ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$';

UPDATE audit_logs
SET channel = request_summary->>'channel'
WHERE channel IS NULL
  AND event_type = 'agent_studio'
  AND request_summary->>'channel' IN ('preview', 'hosted', 'embed', 'api', 'builtin');

UPDATE audit_logs
SET request_summary = agent_audit_redact_jsonb(
        COALESCE(request_summary, '{}'::jsonb)
    ),
    response_summary = agent_audit_redact_jsonb(
        COALESCE(response_summary, '{}'::jsonb)
    ),
    redaction_state = COALESCE(redaction_state, '{}'::jsonb)
        || '{"sensitive_fields":"removed"}'::jsonb
WHERE event_type = 'agent_studio';

CREATE INDEX IF NOT EXISTS idx_audit_logs_agent_dimensions
    ON audit_logs (
        tenant_id, agent_id, agent_version_id, publication_id, channel, created_at DESC, id DESC
    )
    WHERE agent_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS agent_governance_policies (
    tenant_id VARCHAR(255) NOT NULL,
    agent_id UUID NOT NULL,
    trace_retention_days INTEGER NOT NULL DEFAULT 90
        CHECK (trace_retention_days BETWEEN 1 AND 3650),
    runtime_retention_days INTEGER NOT NULL DEFAULT 30
        CHECK (runtime_retention_days BETWEEN 1 AND 3650),
    attachment_retention_days INTEGER NOT NULL DEFAULT 1
        CHECK (attachment_retention_days BETWEEN 1 AND 3650),
    legal_hold BOOLEAN NOT NULL DEFAULT FALSE,
    principal_requests_per_minute INTEGER NOT NULL DEFAULT 30
        CHECK (principal_requests_per_minute > 0),
    principal_requests_per_day INTEGER NOT NULL DEFAULT 1000
        CHECK (principal_requests_per_day > 0),
    ip_requests_per_minute INTEGER NOT NULL DEFAULT 60
        CHECK (ip_requests_per_minute > 0),
    ip_requests_per_day INTEGER NOT NULL DEFAULT 2000
        CHECK (ip_requests_per_day > 0),
    publication_requests_per_minute INTEGER NOT NULL DEFAULT 300
        CHECK (publication_requests_per_minute > 0),
    publication_requests_per_day INTEGER NOT NULL DEFAULT 10000
        CHECK (publication_requests_per_day > 0),
    max_agents_per_tenant INTEGER NOT NULL DEFAULT 100
        CHECK (max_agents_per_tenant > 0),
    max_active_publications INTEGER NOT NULL DEFAULT 10
        CHECK (max_active_publications > 0),
    max_concurrent_runs INTEGER NOT NULL DEFAULT 25
        CHECK (max_concurrent_runs > 0),
    max_daily_tokens BIGINT NOT NULL DEFAULT 10000000
        CHECK (max_daily_tokens > 0),
    max_daily_mcp_calls BIGINT NOT NULL DEFAULT 100000
        CHECK (max_daily_mcp_calls > 0),
    max_storage_bytes BIGINT NOT NULL DEFAULT 10737418240
        CHECK (max_storage_bytes > 0),
    alert_threshold_percent INTEGER NOT NULL DEFAULT 90
        CHECK (alert_threshold_percent BETWEEN 1 AND 100),
    cache_epoch BIGINT NOT NULL DEFAULT 0 CHECK (cache_epoch >= 0),
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_id),
    CONSTRAINT agent_governance_policies_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT
);

ALTER TABLE agent_governance_policies
    ADD COLUMN IF NOT EXISTS max_agents_per_tenant INTEGER NOT NULL DEFAULT 100,
    ADD COLUMN IF NOT EXISTS max_active_publications INTEGER NOT NULL DEFAULT 10,
    ADD COLUMN IF NOT EXISTS max_concurrent_runs INTEGER NOT NULL DEFAULT 25,
    ADD COLUMN IF NOT EXISTS max_daily_tokens BIGINT NOT NULL DEFAULT 10000000,
    ADD COLUMN IF NOT EXISTS max_daily_mcp_calls BIGINT NOT NULL DEFAULT 100000,
    ADD COLUMN IF NOT EXISTS max_storage_bytes BIGINT NOT NULL DEFAULT 10737418240;

DO $$
DECLARE
    column_name TEXT;
    constraint_name TEXT;
BEGIN
    FOREACH column_name IN ARRAY ARRAY[
        'max_agents_per_tenant', 'max_active_publications',
        'max_concurrent_runs', 'max_daily_tokens',
        'max_daily_mcp_calls', 'max_storage_bytes'
    ] LOOP
        constraint_name := 'agent_governance_' || column_name || '_check';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = constraint_name
              AND conrelid = 'agent_governance_policies'::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE agent_governance_policies ADD CONSTRAINT %I CHECK (%I > 0)',
                constraint_name,
                column_name
            );
        END IF;
    END LOOP;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_governance_legal_hold
    ON agent_governance_policies(tenant_id, legal_hold, updated_at DESC);

CREATE TABLE IF NOT EXISTS agent_data_deletion_requests (
    deletion_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(255) NOT NULL,
    agent_id UUID NOT NULL,
    scope VARCHAR(16) NOT NULL CHECK (scope IN ('retention', 'user', 'tenant')),
    subject_user_id VARCHAR(255),
    idempotency_key VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'completed', 'failed', 'blocked')),
    object_keys JSONB NOT NULL DEFAULT '[]'::jsonb,
    deleted_counts JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_code VARCHAR(128),
    requested_by VARCHAR(255) NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    UNIQUE (tenant_id, agent_id, scope, idempotency_key),
    CONSTRAINT agent_data_deletion_requests_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_data_deletion_subject_check
        CHECK ((scope = 'user' AND subject_user_id IS NOT NULL)
            OR (scope <> 'user' AND subject_user_id IS NULL))
);

ALTER TABLE agent_data_deletion_requests
    ADD COLUMN IF NOT EXISTS attempt_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_attempt_at TIMESTAMPTZ;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'agent_data_deletion_attempt_count_check'
          AND conrelid = 'agent_data_deletion_requests'::regclass
    ) THEN
        ALTER TABLE agent_data_deletion_requests
            ADD CONSTRAINT agent_data_deletion_attempt_count_check
            CHECK (attempt_count >= 0);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_data_deletion_status
    ON agent_data_deletion_requests(tenant_id, agent_id, status, requested_at DESC);

CREATE OR REPLACE FUNCTION agent_data_deletion_terminal_guard()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'AGENT_DATA_DELETION_RECEIPT_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    IF OLD.status IN ('completed', 'blocked') THEN
        RAISE EXCEPTION 'AGENT_DATA_DELETION_RECEIPT_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    IF NEW.tenant_id <> OLD.tenant_id
       OR NEW.agent_id <> OLD.agent_id
       OR NEW.scope <> OLD.scope
       OR NEW.subject_user_id IS DISTINCT FROM OLD.subject_user_id
       OR NEW.idempotency_key <> OLD.idempotency_key
       OR NEW.requested_by <> OLD.requested_by
       OR NEW.requested_at <> OLD.requested_at THEN
        RAISE EXCEPTION 'AGENT_DATA_DELETION_IDENTITY_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_data_deletion_requests_guard
    ON agent_data_deletion_requests;
CREATE TRIGGER agent_data_deletion_requests_guard
    BEFORE UPDATE OR DELETE ON agent_data_deletion_requests
    FOR EACH ROW EXECUTE FUNCTION agent_data_deletion_terminal_guard();

-- Tenant closure retains the immutable Agent row but removes its mutable ACL.
-- Preserve the ordinary last-owner invariant while allowing that explicit,
-- already-deleted state to complete in one transaction.
CREATE OR REPLACE FUNCTION agent_studio_protect_last_owner()
RETURNS TRIGGER AS $$
DECLARE
    owner_count INTEGER;
    replacement_owner_id VARCHAR(255);
    agent_deleted_at TIMESTAMPTZ;
BEGIN
    IF OLD.role <> 'owner' THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.role = 'owner' THEN
        RETURN NEW;
    END IF;

    SELECT deleted_at INTO agent_deleted_at
    FROM agents
    WHERE tenant_id = OLD.tenant_id AND agent_id = OLD.agent_id
    FOR UPDATE;
    IF NOT FOUND OR agent_deleted_at IS NOT NULL THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;

    SELECT COUNT(*) INTO owner_count
    FROM agent_members
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND role = 'owner';

    IF owner_count <= 1 THEN
        RAISE EXCEPTION 'AGENT_LAST_OWNER'
            USING ERRCODE = '23514';
    END IF;
    SELECT principal_id INTO replacement_owner_id
    FROM agent_members
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND role = 'owner'
      AND NOT (
          principal_type = OLD.principal_type
          AND principal_id = OLD.principal_id
      )
    ORDER BY created_at, principal_id
    LIMIT 1;
    UPDATE agents
    SET owner_id = replacement_owner_id
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND owner_id = OLD.principal_id;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_members_last_owner ON agent_members;
CREATE TRIGGER agent_members_last_owner
    BEFORE DELETE OR UPDATE OF role ON agent_members
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_last_owner();

CREATE OR REPLACE FUNCTION agent_studio_assert_owner_invariant()
RETURNS TRIGGER AS $$
DECLARE
    target_tenant_id VARCHAR(255);
    target_agent_id UUID;
    current_owner_id VARCHAR(255);
    current_deleted_at TIMESTAMPTZ;
BEGIN
    target_tenant_id := COALESCE(NEW.tenant_id, OLD.tenant_id);
    target_agent_id := COALESCE(NEW.agent_id, OLD.agent_id);

    SELECT owner_id, deleted_at INTO current_owner_id, current_deleted_at
    FROM agents
    WHERE tenant_id = target_tenant_id
      AND agent_id = target_agent_id;
    IF NOT FOUND OR current_deleted_at IS NOT NULL THEN
        RETURN NULL;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM agent_members
        WHERE tenant_id = target_tenant_id
          AND agent_id = target_agent_id
          AND principal_type = 'user'
          AND principal_id = current_owner_id
          AND role = 'owner'
    ) THEN
        RAISE EXCEPTION 'AGENT_OWNER_INVARIANT'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agents_owner_invariant ON agents;
CREATE CONSTRAINT TRIGGER agents_owner_invariant
    AFTER INSERT OR UPDATE OR DELETE ON agents
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION agent_studio_assert_owner_invariant();

DROP TRIGGER IF EXISTS agent_members_owner_invariant ON agent_members;
CREATE CONSTRAINT TRIGGER agent_members_owner_invariant
    AFTER INSERT OR UPDATE OR DELETE ON agent_members
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION agent_studio_assert_owner_invariant();

ALTER TABLE agent_runtime_attachments
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deletion_id UUID,
    ADD COLUMN IF NOT EXISTS cleanup_error VARCHAR(255);

CREATE INDEX IF NOT EXISTS idx_agent_runtime_attachments_active_expiry
    ON agent_runtime_attachments(tenant_id, publication_id, expires_at, created_at)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_agent_traces_publication_channel_created
    ON agent_traces(tenant_id, agent_id, publication_id, channel, created_at DESC)
    WHERE agent_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_sessions_governance
    ON sessions(tenant_id, agent_id, user_id, created_at DESC)
    WHERE agent_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_runs_governance
    ON assistant_runs(tenant_id, agent_id, user_id, created_at DESC)
    WHERE agent_id IS NOT NULL;

COMMIT;
