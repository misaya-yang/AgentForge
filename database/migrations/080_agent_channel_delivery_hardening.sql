-- AS-07 Critic hardening: terminal idempotency results and trusted attachments.
-- Forward-only and additive; existing pending reservations remain non-replayable.

ALTER TABLE agent_runtime_idempotency
    ADD COLUMN IF NOT EXISTS status VARCHAR(16) NOT NULL DEFAULT 'pending',
    ADD COLUMN IF NOT EXISTS response_body BYTEA,
    ADD COLUMN IF NOT EXISTS response_media_type VARCHAR(255),
    ADD COLUMN IF NOT EXISTS response_status_code INTEGER,
    ADD COLUMN IF NOT EXISTS completed_at TIMESTAMPTZ;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_runtime_idempotency_status_check'
    ) THEN
        ALTER TABLE agent_runtime_idempotency
            ADD CONSTRAINT agent_runtime_idempotency_status_check
            CHECK (status IN ('pending', 'completed', 'failed'));
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS agent_runtime_attachments (
    tenant_id VARCHAR(255) NOT NULL,
    attachment_id UUID NOT NULL DEFAULT gen_random_uuid(),
    publication_id UUID NOT NULL,
    principal_id VARCHAR(255) NOT NULL,
    channel VARCHAR(16) NOT NULL CHECK (channel IN ('hosted', 'embed', 'api')),
    storage_key TEXT NOT NULL,
    filename VARCHAR(255) NOT NULL,
    mime_type VARCHAR(255) NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '24 hours'),
    PRIMARY KEY (tenant_id, attachment_id),
    CONSTRAINT agent_runtime_attachments_publication_fk
        FOREIGN KEY (tenant_id, publication_id)
        REFERENCES agent_publications(tenant_id, publication_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_runtime_attachments_lookup
    ON agent_runtime_attachments (
        tenant_id, publication_id, principal_id, channel, attachment_id
    );

CREATE INDEX IF NOT EXISTS idx_agent_runtime_attachments_expiry
    ON agent_runtime_attachments(expires_at);
