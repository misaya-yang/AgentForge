
                CREATE TABLE agents (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_id UUID NOT NULL,
                    owner_id VARCHAR(255) NOT NULL,
                    status VARCHAR(32) NOT NULL DEFAULT 'active',
                    current_draft_id UUID,
                    deleted_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, agent_id)
                );
                CREATE TABLE users (
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    PRIMARY KEY (tenant_id, user_id)
                );
                CREATE TABLE agent_members (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_id UUID NOT NULL,
                    principal_type VARCHAR(32) NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    role VARCHAR(16) NOT NULL,
                    PRIMARY KEY (tenant_id, agent_id, principal_type, principal_id)
                );
                CREATE TABLE agent_publications (
                    tenant_id VARCHAR(255) NOT NULL,
                    publication_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    channel VARCHAR(16) NOT NULL,
                    status VARCHAR(16) NOT NULL DEFAULT 'active',
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, publication_id)
                );
                CREATE TABLE agent_drafts (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 1,
                    spec JSONB NOT NULL DEFAULT '{}'::jsonb,
                    spec_hash CHAR(64) NOT NULL DEFAULT repeat('0', 64),
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, draft_id)
                );
                CREATE TABLE agent_versions (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, agent_version_id)
                );
                CREATE TABLE agent_version_capabilities (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    capability_type VARCHAR(32) NOT NULL,
                    resource_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE agent_draft_knowledge_bindings (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    dataset_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE agent_draft_skill_bindings (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    skill_version_id UUID NOT NULL
                );
                CREATE TABLE mcp_channel_grants (
                    tenant_id VARCHAR(255) NOT NULL,
                    tool_id UUID NOT NULL,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE audit_logs (
                    id BIGSERIAL PRIMARY KEY,
                    event_type VARCHAR(100) NOT NULL,
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    resource_type VARCHAR(100),
                    resource_id VARCHAR(255),
                    action VARCHAR(50) NOT NULL,
                    request_summary JSONB,
                    response_summary JSONB,
                    status VARCHAR(50) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE sessions (
                    session_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255),
                    user_id VARCHAR(255),
                    agent_id UUID,
                    agent_version_id UUID,
                    agent_draft_revision INTEGER,
                    publication_id UUID,
                    channel VARCHAR(16),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_runs (
                    run_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    agent_id UUID,
                    status VARCHAR(32) NOT NULL DEFAULT 'running',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_run_checkpoints (
                    checkpoint_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    agent_id UUID,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_traces (
                    trace_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255),
                    agent_id UUID,
                    agent_version_id UUID,
                    publication_id UUID,
                    channel VARCHAR(16),
                    total_tokens BIGINT NOT NULL DEFAULT 0,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_trace_spans (
                    span_id UUID PRIMARY KEY,
                    trace_id UUID NOT NULL,
                    span_kind VARCHAR(64) NOT NULL,
                    name VARCHAR(160) NOT NULL,
                    attributes JSONB NOT NULL DEFAULT '{}'::jsonb
                );
                CREATE TABLE agent_runtime_attachments (
                    tenant_id VARCHAR(255) NOT NULL,
                    attachment_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    channel VARCHAR(16) NOT NULL,
                    storage_key TEXT NOT NULL,
                    filename VARCHAR(255) NOT NULL DEFAULT 'fixture.txt',
                    mime_type VARCHAR(255) NOT NULL DEFAULT 'text/plain',
                    size_bytes BIGINT NOT NULL DEFAULT 1,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    expires_at TIMESTAMPTZ NOT NULL,
                    PRIMARY KEY (tenant_id, attachment_id)
                );
                CREATE TABLE agent_runtime_idempotency (
                    tenant_id VARCHAR(255) NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    idempotency_key VARCHAR(255) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, publication_id, principal_id, idempotency_key)
                );
                CREATE TABLE agent_runtime_feedback (
                    tenant_id VARCHAR(255) NOT NULL,
                    feedback_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, feedback_id)
                );
                CREATE TABLE session_memory (
                    tenant_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    key VARCHAR(255) NOT NULL
                );
                CREATE TABLE user_memory (
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    key VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_memory_sources (
                    source_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE assistant_memory_reflections (
                    reflection_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE semantic_cache (
                    id BIGSERIAL PRIMARY KEY,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_api_tokens (
                    tenant_id VARCHAR(255) NOT NULL,
                    token_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    created_by VARCHAR(255) NOT NULL,
                    revoked_at TIMESTAMPTZ,
                    PRIMARY KEY (tenant_id, token_id)
                );
                CREATE TABLE mcp_connections (
                    tenant_id VARCHAR(255) NOT NULL,
                    connection_id UUID NOT NULL,
                    owner_user_id VARCHAR(255),
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    revoked_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, connection_id)
                );
                CREATE TABLE connector_credential_principals (
                    tenant_id VARCHAR(255) NOT NULL,
                    grant_id UUID NOT NULL,
                    owner_user_id VARCHAR(255),
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    revoked_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, grant_id)
                );
                