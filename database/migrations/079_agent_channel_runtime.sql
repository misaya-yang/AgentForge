-- AS-07: delivery-channel token lifecycle, idempotency and feedback.
-- Forward-only and additive so AS-06 immutable Versions/Publications remain unchanged.

ALTER TABLE agent_api_tokens
    ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS rotated_from_token_id UUID;

CREATE INDEX IF NOT EXISTS idx_agent_api_tokens_hash_active
    ON agent_api_tokens(token_hash)
    WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS agent_runtime_idempotency (
    tenant_id VARCHAR(255) NOT NULL,
    publication_id UUID NOT NULL,
    principal_id VARCHAR(255) NOT NULL,
    idempotency_key VARCHAR(255) NOT NULL,
    request_hash CHAR(64) NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    session_id VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '24 hours'),
    PRIMARY KEY (tenant_id, publication_id, principal_id, idempotency_key),
    CONSTRAINT agent_runtime_idempotency_publication_fk
        FOREIGN KEY (tenant_id, publication_id)
        REFERENCES agent_publications(tenant_id, publication_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_runtime_idempotency_expiry
    ON agent_runtime_idempotency(expires_at);

CREATE TABLE IF NOT EXISTS agent_runtime_feedback (
    tenant_id VARCHAR(255) NOT NULL,
    feedback_id UUID NOT NULL DEFAULT gen_random_uuid(),
    publication_id UUID NOT NULL,
    agent_version_id UUID NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    principal_id VARCHAR(255) NOT NULL,
    rating SMALLINT NOT NULL CHECK (rating IN (-1, 1)),
    comment TEXT NOT NULL DEFAULT '',
    channel VARCHAR(16) NOT NULL CHECK (channel IN ('hosted', 'embed', 'api')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, feedback_id),
    CONSTRAINT agent_runtime_feedback_publication_fk
        FOREIGN KEY (tenant_id, publication_id)
        REFERENCES agent_publications(tenant_id, publication_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_runtime_feedback_version_fk
        FOREIGN KEY (tenant_id, agent_version_id)
        REFERENCES agent_versions(tenant_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_runtime_feedback_once
        UNIQUE (tenant_id, publication_id, session_id, principal_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_runtime_feedback_publication_created
    ON agent_runtime_feedback(tenant_id, publication_id, created_at DESC);

