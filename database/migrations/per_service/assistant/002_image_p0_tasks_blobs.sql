-- =============================================================================
-- Migration: assistant/002_image_p0_tasks_blobs
-- Owner    : gateway / capability-worker
-- Purpose  : Gemini image P0 primitives:
--            * object-store-first image blob references
--            * durable Postgres task queue/state table
--            * stronger image_turns metadata for Gemini replay/audit
-- Idempotent: every DDL gated by IF NOT EXISTS / ADD COLUMN IF NOT EXISTS.
-- =============================================================================

ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS content_sha256 VARCHAR(64);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS byte_size BIGINT;

CREATE INDEX IF NOT EXISTS idx_artifacts_content_sha256
    ON assistant.artifacts(content_sha256)
    WHERE content_sha256 IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 1. image_blobs: object-store-first data plane references
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant.image_blobs (
    blob_id          VARCHAR(64) PRIMARY KEY,
    owner_scope      VARCHAR(255) NOT NULL,
    content_sha256   VARCHAR(64),
    byte_size        BIGINT,
    mime_type        VARCHAR(128) NOT NULL,
    storage_key      TEXT NOT NULL,
    source           VARCHAR(64) NOT NULL,
    status           VARCHAR(32) NOT NULL DEFAULT 'pending_upload',
    artifact_id      VARCHAR(64),
    error            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_image_blobs_owner
    ON assistant.image_blobs(owner_scope, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_image_blobs_sha
    ON assistant.image_blobs(owner_scope, content_sha256)
    WHERE content_sha256 IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_image_blobs_status
    ON assistant.image_blobs(status, created_at DESC);

COMMENT ON TABLE assistant.image_blobs IS
    'Object-store-first image data plane references. Requests pass blob_id instead of image bytes.';

-- ---------------------------------------------------------------------------
-- 2. image_tasks: durable task queue + replayable task state
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant.image_tasks (
    task_id             VARCHAR(64) PRIMARY KEY,
    tenant_id           VARCHAR(64),
    user_id             VARCHAR(64),
    runtime_scope_version SMALLINT NOT NULL DEFAULT 0,
    owner_scope         VARCHAR(255) NOT NULL,
    status              VARCHAR(32) NOT NULL DEFAULT 'pending',
    prompt              TEXT NOT NULL,
    model_id            VARCHAR(128) NOT NULL,
    request_payload     JSONB NOT NULL DEFAULT '{}'::jsonb,
    result              JSONB,
    progress            INT NOT NULL DEFAULT 0,
    provider            VARCHAR(64),
    error               TEXT,
    error_code          VARCHAR(64),
    turn_id             VARCHAR(64),
    session_id          VARCHAR(255),
    parent_artifact_id  VARCHAR(64),
    output_artifact_id  VARCHAR(64),
    client_request_id   VARCHAR(128),
    request_hash        VARCHAR(64),
    attempt_count       INT NOT NULL DEFAULT 0,
    max_attempts        INT NOT NULL DEFAULT 3,
    locked_until        TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at          TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_image_tasks_queue
    ON assistant.image_tasks(status, locked_until, created_at)
    WHERE status IN ('pending', 'running');

CREATE INDEX IF NOT EXISTS idx_image_tasks_owner_status
    ON assistant.image_tasks(owner_scope, status, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_image_tasks_turn
    ON assistant.image_tasks(turn_id)
    WHERE turn_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_image_tasks_session
    ON assistant.image_tasks(session_id, created_at DESC)
    WHERE session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_image_tasks_client_request
    ON assistant.image_tasks(owner_scope, client_request_id)
    WHERE client_request_id IS NOT NULL;

COMMENT ON TABLE assistant.image_tasks IS
    'Durable image generation task queue/state. Workers claim pending rows with FOR UPDATE SKIP LOCKED.';

-- ---------------------------------------------------------------------------
-- 3. image_turns: Gemini replay/audit fields. Existing status remains for
--    compatibility; state mirrors it and allows the explicit state-machine
--    vocabulary to evolve without breaking old consumers.
-- ---------------------------------------------------------------------------
ALTER TABLE assistant.image_turns ADD COLUMN IF NOT EXISTS thought_signature TEXT;
ALTER TABLE assistant.image_turns ADD COLUMN IF NOT EXISTS provider_text TEXT;
ALTER TABLE assistant.image_turns ADD COLUMN IF NOT EXISTS output_artifact_ids JSONB;
ALTER TABLE assistant.image_turns ADD COLUMN IF NOT EXISTS state VARCHAR(32);

UPDATE assistant.image_turns
SET state = status
WHERE state IS NULL;

CREATE INDEX IF NOT EXISTS idx_image_turns_state
    ON assistant.image_turns(state, created_at DESC);
