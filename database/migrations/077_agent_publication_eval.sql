-- Migration: 077_agent_publication_eval.sql
-- Goal: bind Agent release evaluation to an exact Draft/runtime fingerprint,
--       make release retries durable, and preserve append-only promotion evidence.

BEGIN;

CREATE TABLE IF NOT EXISTS agent_release_evaluations (
    tenant_id VARCHAR(255) NOT NULL,
    evaluation_id UUID NOT NULL DEFAULT gen_random_uuid(),
    agent_id UUID NOT NULL,
    draft_id UUID NOT NULL,
    draft_revision INTEGER NOT NULL CHECK (draft_revision >= 1),
    spec_hash CHAR(64) NOT NULL CHECK (spec_hash ~ '^[0-9a-f]{64}$'),
    runtime_fingerprint JSONB NOT NULL,
    runtime_fingerprint_hash CHAR(64) NOT NULL
        CHECK (runtime_fingerprint_hash ~ '^[0-9a-f]{64}$'),
    release_identity_hash CHAR(64) NOT NULL
        CHECK (release_identity_hash ~ '^[0-9a-f]{64}$'),
    profile_id VARCHAR(64) NOT NULL,
    profile_version VARCHAR(64) NOT NULL,
    dataset_id UUID,
    experiment_run_id UUID,
    channel VARCHAR(16) NOT NULL CHECK (channel IN ('hosted', 'embed', 'api')),
    auth_mode VARCHAR(16) NOT NULL DEFAULT 'private'
        CHECK (auth_mode IN ('private', 'tenant', 'public', 'token')),
    channel_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    channel_policy_hash CHAR(64) NOT NULL
        CHECK (channel_policy_hash ~ '^[0-9a-f]{64}$'),
    status VARCHAR(16) NOT NULL
        CHECK (status IN ('passed', 'failed', 'cancelled')),
    validation_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    gate_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, evaluation_id),
    CONSTRAINT agent_release_evaluations_agent_key
        UNIQUE (tenant_id, agent_id, evaluation_id),
    CONSTRAINT agent_release_evaluations_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_evaluations_draft_fk
        FOREIGN KEY (tenant_id, agent_id, draft_id)
        REFERENCES agent_drafts(tenant_id, agent_id, draft_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_evaluations_dataset_fk
        FOREIGN KEY (dataset_id)
        REFERENCES eval_datasets(dataset_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_evaluations_run_fk
        FOREIGN KEY (experiment_run_id)
        REFERENCES eval_experiment_runs(run_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_release_evaluations_agent_created
    ON agent_release_evaluations(tenant_id, agent_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_release_evaluations_draft
    ON agent_release_evaluations(
        tenant_id, agent_id, draft_id, draft_revision, runtime_fingerprint_hash
    );

CREATE TABLE IF NOT EXISTS agent_release_evaluation_events (
    tenant_id VARCHAR(255) NOT NULL,
    event_id UUID NOT NULL DEFAULT gen_random_uuid(),
    evaluation_id UUID NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    status VARCHAR(16) NOT NULL
        CHECK (status IN ('queued', 'running', 'passed', 'failed', 'cancelled', 'stale')),
    summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, event_id),
    CONSTRAINT agent_release_evaluation_events_sequence_unique
        UNIQUE (tenant_id, evaluation_id, sequence),
    CONSTRAINT agent_release_evaluation_events_eval_fk
        FOREIGN KEY (tenant_id, evaluation_id)
        REFERENCES agent_release_evaluations(tenant_id, evaluation_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_release_evaluation_events_eval
    ON agent_release_evaluation_events(tenant_id, evaluation_id, sequence);

ALTER TABLE agent_versions
    ADD COLUMN IF NOT EXISTS release_evaluation_id UUID,
    ADD COLUMN IF NOT EXISTS release_identity_hash CHAR(64);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_versions_release_identity_hash_check'
          AND conrelid = 'agent_versions'::regclass
    ) THEN
        ALTER TABLE agent_versions
            ADD CONSTRAINT agent_versions_release_identity_hash_check
            CHECK (
                release_identity_hash IS NULL
                OR release_identity_hash ~ '^[0-9a-f]{64}$'
            );
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_versions_release_evaluation_fk'
          AND conrelid = 'agent_versions'::regclass
    ) THEN
        ALTER TABLE agent_versions
            ADD CONSTRAINT agent_versions_release_evaluation_fk
            FOREIGN KEY (tenant_id, agent_id, release_evaluation_id)
            REFERENCES agent_release_evaluations(tenant_id, agent_id, evaluation_id)
            ON DELETE RESTRICT;
    END IF;
END;
$$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_versions_release_identity_unique
    ON agent_versions(tenant_id, agent_id, release_identity_hash)
    WHERE release_identity_hash IS NOT NULL;

ALTER TABLE agent_publish_events
    ADD COLUMN IF NOT EXISTS operation VARCHAR(16) NOT NULL DEFAULT 'promote',
    ADD COLUMN IF NOT EXISTS release_evaluation_id UUID,
    ADD COLUMN IF NOT EXISTS request_hash CHAR(64);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_publish_events_operation_check'
          AND conrelid = 'agent_publish_events'::regclass
    ) THEN
        ALTER TABLE agent_publish_events
            ADD CONSTRAINT agent_publish_events_operation_check
            CHECK (operation IN ('promote', 'rollback'));
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_publish_events_request_hash_check'
          AND conrelid = 'agent_publish_events'::regclass
    ) THEN
        ALTER TABLE agent_publish_events
            ADD CONSTRAINT agent_publish_events_request_hash_check
            CHECK (request_hash IS NULL OR request_hash ~ '^[0-9a-f]{64}$');
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_publish_events_release_evaluation_fk'
          AND conrelid = 'agent_publish_events'::regclass
    ) THEN
        ALTER TABLE agent_publish_events
            ADD CONSTRAINT agent_publish_events_release_evaluation_fk
            FOREIGN KEY (tenant_id, agent_id, release_evaluation_id)
            REFERENCES agent_release_evaluations(tenant_id, agent_id, evaluation_id)
            ON DELETE RESTRICT;
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS agent_release_requests (
    tenant_id VARCHAR(255) NOT NULL,
    operation VARCHAR(16) NOT NULL CHECK (operation IN ('promote', 'rollback')),
    idempotency_key_hash CHAR(64) NOT NULL
        CHECK (idempotency_key_hash ~ '^[0-9a-f]{64}$'),
    request_hash CHAR(64) NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    agent_id UUID NOT NULL,
    evaluation_id UUID,
    result_version_id UUID NOT NULL,
    result_publication_id UUID NOT NULL,
    result_event_id UUID NOT NULL,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, operation, idempotency_key_hash),
    CONSTRAINT agent_release_requests_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_requests_eval_fk
        FOREIGN KEY (tenant_id, agent_id, evaluation_id)
        REFERENCES agent_release_evaluations(tenant_id, agent_id, evaluation_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_requests_version_fk
        FOREIGN KEY (tenant_id, agent_id, result_version_id)
        REFERENCES agent_versions(tenant_id, agent_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_requests_publication_fk
        FOREIGN KEY (tenant_id, result_publication_id, agent_id)
        REFERENCES agent_publications(tenant_id, publication_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_release_requests_event_fk
        FOREIGN KEY (tenant_id, result_event_id)
        REFERENCES agent_publish_events(tenant_id, event_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_release_requests_agent_created
    ON agent_release_requests(tenant_id, agent_id, created_at DESC);

CREATE OR REPLACE FUNCTION agent_studio_reject_release_evidence_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'AGENT_RELEASE_EVIDENCE_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_release_evaluations_immutable
    ON agent_release_evaluations;
CREATE TRIGGER agent_release_evaluations_immutable
    BEFORE UPDATE OR DELETE ON agent_release_evaluations
    FOR EACH ROW EXECUTE FUNCTION agent_studio_reject_release_evidence_mutation();

DROP TRIGGER IF EXISTS agent_release_evaluation_events_immutable
    ON agent_release_evaluation_events;
CREATE TRIGGER agent_release_evaluation_events_immutable
    BEFORE UPDATE OR DELETE ON agent_release_evaluation_events
    FOR EACH ROW EXECUTE FUNCTION agent_studio_reject_release_evidence_mutation();

DROP TRIGGER IF EXISTS agent_release_requests_immutable
    ON agent_release_requests;
CREATE TRIGGER agent_release_requests_immutable
    BEFORE UPDATE OR DELETE ON agent_release_requests
    FOR EACH ROW EXECUTE FUNCTION agent_studio_reject_release_evidence_mutation();

COMMIT;
