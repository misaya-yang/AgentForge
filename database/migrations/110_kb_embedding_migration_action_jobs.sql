-- T3 P1: durable control-plane jobs for embedding backfill / verify / gate.
--
-- These actions may exceed an HTTP/proxy timeout and must survive client
-- disconnects and API-process restarts. PostgreSQL is the sole queue/state
-- authority; worker processes claim rows with SKIP LOCKED + a lease token.
-- Migration numbers 106-109 are owned by other RAG upgrade workstreams.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migrations'::regclass
          AND conname = 'uq_kb_embedding_migration_dataset'
    ) THEN
        ALTER TABLE embedding_migrations
            ADD CONSTRAINT uq_kb_embedding_migration_dataset
            UNIQUE (migration_id, dataset_id);
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS embedding_migration_action_jobs (
    job_id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    migration_id     UUID NOT NULL,
    dataset_id       VARCHAR(255) NOT NULL,
    action           VARCHAR(20) NOT NULL
        CHECK (action IN ('backfill', 'verify', 'gate')),
    request_hash     CHAR(64) NOT NULL
        CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    state            VARCHAR(20) NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued', 'running', 'succeeded', 'failed')),
    payload          JSONB NOT NULL DEFAULT '{}'::jsonb,
    result           JSONB,
    error            TEXT,
    requested_by     VARCHAR(255),
    attempt_count    INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    claimed_by       VARCHAR(255),
    claim_token      UUID,
    terminal_claim_token UUID,
    available_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_expires_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    started_at       TIMESTAMPTZ,
    finished_at      TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_kb_embedding_action_payload_object
        CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT chk_kb_embedding_action_result_object
        CHECK (result IS NULL OR jsonb_typeof(result) = 'object'),
    CONSTRAINT chk_kb_embedding_action_claim_shape
        CHECK (
            (
                state = 'running'
                AND claim_token IS NOT NULL
                AND claimed_by IS NOT NULL
                AND lease_expires_at IS NOT NULL
            ) OR (
                state <> 'running'
                AND claim_token IS NULL
                AND claimed_by IS NULL
                AND lease_expires_at IS NULL
            )
        ),
    CONSTRAINT fk_kb_embedding_action_migration_dataset
        FOREIGN KEY (migration_id, dataset_id)
        REFERENCES embedding_migrations (migration_id, dataset_id)
    ON DELETE CASCADE
);

DO $$
BEGIN
    IF (
        SELECT n.nspname
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.oid = 'knowledge.embedding_migration_action_jobs'::regclass
    ) <> 'knowledge' THEN
        RAISE EXCEPTION
            'embedding_migration_action_jobs must belong to knowledge schema';
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migration_action_jobs'::regclass
          AND conname = 'fk_kb_embedding_action_migration_dataset'
    ) THEN
        ALTER TABLE embedding_migration_action_jobs
            ADD CONSTRAINT fk_kb_embedding_action_migration_dataset
            FOREIGN KEY (migration_id, dataset_id)
            REFERENCES embedding_migrations (migration_id, dataset_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migration_action_jobs'::regclass
          AND conname = 'chk_kb_embedding_action_claim_shape'
    ) THEN
        ALTER TABLE embedding_migration_action_jobs
            ADD CONSTRAINT chk_kb_embedding_action_claim_shape
            CHECK (
                (
                    state = 'running'
                    AND claim_token IS NOT NULL
                    AND claimed_by IS NOT NULL
                    AND lease_expires_at IS NOT NULL
                ) OR (
                    state <> 'running'
                    AND claim_token IS NULL
                    AND claimed_by IS NULL
                    AND lease_expires_at IS NULL
                )
            )
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE embedding_migration_action_jobs
    VALIDATE CONSTRAINT fk_kb_embedding_action_migration_dataset;
ALTER TABLE embedding_migration_action_jobs
    VALIDATE CONSTRAINT chk_kb_embedding_action_claim_shape;

-- One action at a time per dataset, including the narrow window where an old
-- migration has reached a terminal state but its worker receipt is not yet
-- committed. Same-action concurrent submissions return this row; a different
-- action or migration receives a deterministic conflict.
CREATE UNIQUE INDEX IF NOT EXISTS idx_kb_embedding_action_jobs_one_active
    ON embedding_migration_action_jobs (dataset_id)
    WHERE state IN ('queued', 'running');

CREATE INDEX IF NOT EXISTS idx_kb_embedding_action_jobs_claim
    ON embedding_migration_action_jobs (state, available_at, lease_expires_at, created_at);

CREATE INDEX IF NOT EXISTS idx_kb_embedding_action_jobs_migration_recent
    ON embedding_migration_action_jobs (migration_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_kb_embedding_action_jobs_dataset_recent
    ON embedding_migration_action_jobs (dataset_id, finished_at DESC, created_at DESC);

COMMENT ON TABLE embedding_migration_action_jobs IS
  'T3 durable 202 jobs for backfill/verify/gate; claimed cross-process with token CAS and renewable leases';

COMMIT;
