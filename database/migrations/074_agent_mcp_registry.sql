-- Migration: 074_agent_mcp_registry.sql
-- Goal: additive tenant-safe Agent Studio MCP registry, immutable discovery
-- snapshots, explicit credential principals and channel grants. Secret values
-- are deliberately absent; only opaque Secret Store references are persisted.

BEGIN;

CREATE TABLE IF NOT EXISTS mcp_servers (
    tenant_id VARCHAR(255) NOT NULL,
    server_id UUID NOT NULL DEFAULT gen_random_uuid(),
    name VARCHAR(128) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    base_url TEXT NOT NULL,
    transport VARCHAR(32) NOT NULL DEFAULT 'streamable_http'
        CHECK (transport = 'streamable_http'),
    auth_method VARCHAR(32) NOT NULL DEFAULT 'none'
        CHECK (auth_method IN ('none', 'bearer', 'oauth')),
    oauth_metadata_url TEXT,
    oauth_resource TEXT,
    oauth_audience TEXT,
    allowed_origins TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    timeout_ms INTEGER NOT NULL DEFAULT 30000
        CHECK (timeout_ms BETWEEN 100 AND 120000),
    max_concurrency INTEGER NOT NULL DEFAULT 5
        CHECK (max_concurrency BETWEEN 1 AND 32),
    response_limit_bytes INTEGER NOT NULL DEFAULT 1048576
        CHECK (response_limit_bytes BETWEEN 1024 AND 8388608),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    health_status VARCHAR(32) NOT NULL DEFAULT 'unknown'
        CHECK (health_status IN ('unknown', 'healthy', 'degraded', 'unavailable')),
    circuit_state VARCHAR(16) NOT NULL DEFAULT 'closed'
        CHECK (circuit_state IN ('closed', 'open', 'half_open')),
    consecutive_failures INTEGER NOT NULL DEFAULT 0
        CHECK (consecutive_failures >= 0),
    circuit_open_until TIMESTAMPTZ,
    last_health_at TIMESTAMPTZ,
    last_error_code VARCHAR(64),
    deleted_at TIMESTAMPTZ,
    created_by VARCHAR(255) NOT NULL,
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, server_id),
    CONSTRAINT mcp_servers_tenant_server_key UNIQUE (tenant_id, server_id),
    CONSTRAINT mcp_servers_allowed_origins_check CHECK (
        cardinality(allowed_origins) BETWEEN 1 AND 64
    ),
    CONSTRAINT mcp_servers_https_url_check CHECK (
        base_url ~ '^https://[^[:space:]]+$'
        AND base_url !~ '@'
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_mcp_servers_tenant_name_live
    ON mcp_servers(tenant_id, LOWER(name))
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_mcp_servers_tenant_health
    ON mcp_servers(tenant_id, enabled, health_status)
    WHERE deleted_at IS NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'mcp_servers_allowed_origins_check'
          AND conrelid = 'mcp_servers'::regclass
    ) THEN
        ALTER TABLE mcp_servers
            ADD CONSTRAINT mcp_servers_allowed_origins_check
            CHECK (cardinality(allowed_origins) BETWEEN 1 AND 64) NOT VALID;
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS mcp_connections (
    tenant_id VARCHAR(255) NOT NULL,
    connection_id UUID NOT NULL DEFAULT gen_random_uuid(),
    server_id UUID NOT NULL,
    principal_type VARCHAR(32) NOT NULL
        CHECK (principal_type IN ('service_account', 'user_delegated')),
    owner_user_id VARCHAR(255),
    secret_ref TEXT,
    scopes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    audience TEXT,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR(255) NOT NULL,
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, connection_id),
    CONSTRAINT mcp_connections_tenant_connection_key
        UNIQUE (tenant_id, connection_id),
    CONSTRAINT mcp_connections_server_fk
        FOREIGN KEY (tenant_id, server_id)
        REFERENCES mcp_servers(tenant_id, server_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_connections_owner_fk
        FOREIGN KEY (tenant_id, owner_user_id)
        REFERENCES users(tenant_id, user_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_connections_principal_owner_check CHECK (
        (principal_type = 'service_account' AND owner_user_id IS NULL)
        OR
        (principal_type = 'user_delegated' AND owner_user_id IS NOT NULL)
    ),
    CONSTRAINT mcp_connections_secret_ref_check CHECK (
        secret_ref IS NULL
        OR (
            secret_ref ~ '^[a-z][a-z0-9+.-]*://[^[:space:]]+$'
            AND secret_ref !~ '^https?://'
        )
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_mcp_connections_active_principal
    ON mcp_connections(
        tenant_id,
        server_id,
        principal_type,
        COALESCE(owner_user_id, '')
    )
    WHERE enabled = TRUE AND revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_mcp_connections_runtime
    ON mcp_connections(tenant_id, server_id, principal_type, owner_user_id)
    WHERE enabled = TRUE AND revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS mcp_tools (
    tenant_id VARCHAR(255) NOT NULL,
    tool_id UUID NOT NULL DEFAULT gen_random_uuid(),
    server_id UUID NOT NULL,
    upstream_name VARCHAR(255) NOT NULL,
    runtime_name VARCHAR(255) NOT NULL,
    current_snapshot_id UUID,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, tool_id),
    CONSTRAINT mcp_tools_tenant_tool_key UNIQUE (tenant_id, tool_id),
    CONSTRAINT mcp_tools_server_name_unique
        UNIQUE (tenant_id, server_id, upstream_name),
    CONSTRAINT mcp_tools_runtime_name_unique UNIQUE (tenant_id, runtime_name),
    CONSTRAINT mcp_tools_server_fk
        FOREIGN KEY (tenant_id, server_id)
        REFERENCES mcp_servers(tenant_id, server_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_tools_runtime_name_check
        CHECK (runtime_name ~ '^mcp_[0-9a-f]{32}__[0-9a-f]{32}$')
);

CREATE TABLE IF NOT EXISTS mcp_tool_snapshots (
    tenant_id VARCHAR(255) NOT NULL,
    snapshot_id UUID NOT NULL DEFAULT gen_random_uuid(),
    server_id UUID NOT NULL,
    tool_id UUID NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    schema_hash CHAR(64) NOT NULL CHECK (schema_hash ~ '^[0-9a-f]{64}$'),
    contract_hash CHAR(64) NOT NULL CHECK (contract_hash ~ '^[0-9a-f]{64}$'),
    description TEXT NOT NULL DEFAULT '',
    input_schema JSONB NOT NULL DEFAULT '{}'::jsonb,
    risk_level VARCHAR(16) NOT NULL DEFAULT 'medium'
        CHECK (risk_level IN ('low', 'medium', 'high', 'critical')),
    read_only BOOLEAN NOT NULL DEFAULT FALSE,
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, snapshot_id),
    CONSTRAINT mcp_tool_snapshots_tenant_snapshot_key
        UNIQUE (tenant_id, snapshot_id),
    CONSTRAINT mcp_tool_snapshots_tenant_tool_snapshot_key
        UNIQUE (tenant_id, tool_id, snapshot_id),
    CONSTRAINT mcp_tool_snapshots_contract_unique
        UNIQUE (tenant_id, tool_id, contract_hash),
    CONSTRAINT mcp_tool_snapshots_version_unique
        UNIQUE (tenant_id, tool_id, schema_version),
    CONSTRAINT mcp_tool_snapshots_server_fk
        FOREIGN KEY (tenant_id, server_id)
        REFERENCES mcp_servers(tenant_id, server_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_tool_snapshots_tool_fk
        FOREIGN KEY (tenant_id, tool_id)
        REFERENCES mcp_tools(tenant_id, tool_id)
        ON DELETE RESTRICT
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'mcp_tools_current_snapshot_fk'
          AND conrelid = 'mcp_tools'::regclass
    ) THEN
        ALTER TABLE mcp_tools
            ADD CONSTRAINT mcp_tools_current_snapshot_fk
            FOREIGN KEY (tenant_id, tool_id, current_snapshot_id)
            REFERENCES mcp_tool_snapshots(tenant_id, tool_id, snapshot_id)
            DEFERRABLE INITIALLY DEFERRED;
    END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_mcp_tool_snapshots_lookup
    ON mcp_tool_snapshots(tenant_id, server_id, tool_id, schema_version DESC);

CREATE TABLE IF NOT EXISTS mcp_schema_diffs (
    tenant_id VARCHAR(255) NOT NULL,
    diff_id UUID NOT NULL DEFAULT gen_random_uuid(),
    server_id UUID NOT NULL,
    tool_id UUID NOT NULL,
    from_snapshot_id UUID,
    to_snapshot_id UUID NOT NULL,
    breaking BOOLEAN NOT NULL DEFAULT FALSE,
    diff JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, diff_id),
    CONSTRAINT mcp_schema_diffs_server_fk
        FOREIGN KEY (tenant_id, server_id)
        REFERENCES mcp_servers(tenant_id, server_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_schema_diffs_tool_fk
        FOREIGN KEY (tenant_id, tool_id)
        REFERENCES mcp_tools(tenant_id, tool_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_schema_diffs_from_fk
        FOREIGN KEY (tenant_id, from_snapshot_id)
        REFERENCES mcp_tool_snapshots(tenant_id, snapshot_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_schema_diffs_to_fk
        FOREIGN KEY (tenant_id, to_snapshot_id)
        REFERENCES mcp_tool_snapshots(tenant_id, snapshot_id)
        ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS mcp_channel_grants (
    tenant_id VARCHAR(255) NOT NULL,
    connection_id UUID NOT NULL,
    tool_id UUID NOT NULL,
    channel VARCHAR(32) NOT NULL
        CHECK (channel IN ('preview', 'hosted_private', 'hosted_public', 'embed', 'api')),
    read_only_only BOOLEAN NOT NULL DEFAULT TRUE,
    approved_schema_hash CHAR(64),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    authorized_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, connection_id, tool_id, channel),
    CONSTRAINT mcp_channel_grants_connection_fk
        FOREIGN KEY (tenant_id, connection_id)
        REFERENCES mcp_connections(tenant_id, connection_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_channel_grants_tool_fk
        FOREIGN KEY (tenant_id, tool_id)
        REFERENCES mcp_tools(tenant_id, tool_id)
        ON DELETE RESTRICT,
    CONSTRAINT mcp_channel_grants_schema_hash_check CHECK (
        approved_schema_hash IS NULL
        OR approved_schema_hash::text ~ '^[0-9a-f]{64}$'
    )
);

ALTER TABLE mcp_channel_grants
    ADD COLUMN IF NOT EXISTS approved_schema_hash CHAR(64);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'mcp_channel_grants_schema_hash_check'
          AND conrelid = 'mcp_channel_grants'::regclass
    ) THEN
        ALTER TABLE mcp_channel_grants
            ADD CONSTRAINT mcp_channel_grants_schema_hash_check
            CHECK (
                approved_schema_hash IS NULL
                OR approved_schema_hash::text ~ '^[0-9a-f]{64}$'
            ) NOT VALID;
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS connector_credential_principals (
    tenant_id VARCHAR(255) NOT NULL,
    grant_id UUID NOT NULL DEFAULT gen_random_uuid(),
    provider VARCHAR(64) NOT NULL CHECK (provider = 'confluence'),
    principal_type VARCHAR(32) NOT NULL
        CHECK (principal_type IN ('service_account', 'user_delegated')),
    owner_user_id VARCHAR(255),
    secret_ref TEXT NOT NULL,
    scopes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    audience TEXT,
    connection_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    allowed_channels TEXT[] NOT NULL DEFAULT ARRAY['preview', 'hosted_private']::TEXT[],
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by VARCHAR(255) NOT NULL,
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, grant_id),
    CONSTRAINT connector_credential_principals_owner_fk
        FOREIGN KEY (tenant_id, owner_user_id)
        REFERENCES users(tenant_id, user_id)
        ON DELETE RESTRICT,
    CONSTRAINT connector_credential_principals_owner_check CHECK (
        (principal_type = 'service_account' AND owner_user_id IS NULL)
        OR
        (principal_type = 'user_delegated' AND owner_user_id IS NOT NULL)
    ),
    CONSTRAINT connector_credential_principals_secret_ref_check CHECK (
        secret_ref ~ '^[a-z][a-z0-9+.-]*://[^[:space:]]+$'
        AND secret_ref !~ '^https?://'
    ),
    CONSTRAINT connector_credential_principals_channels_check CHECK (
        allowed_channels <@ ARRAY[
            'preview', 'hosted_private', 'hosted_public', 'embed', 'api'
        ]::TEXT[]
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_connector_credential_active_principal
    ON connector_credential_principals(
        tenant_id,
        provider,
        principal_type,
        COALESCE(owner_user_id, '')
    )
    WHERE enabled = TRUE AND revoked_at IS NULL;

CREATE OR REPLACE FUNCTION reject_mcp_snapshot_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'MCP tool snapshots are immutable';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS mcp_tool_snapshots_immutable ON mcp_tool_snapshots;
CREATE TRIGGER mcp_tool_snapshots_immutable
    BEFORE UPDATE OR DELETE ON mcp_tool_snapshots
    FOR EACH ROW EXECUTE FUNCTION reject_mcp_snapshot_mutation();

COMMIT;
