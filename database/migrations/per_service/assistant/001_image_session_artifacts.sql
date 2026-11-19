-- =============================================================================
-- Migration: assistant/001_image_session_artifacts
-- Owner    : gateway / capability-worker
-- Purpose  : First-class multi-turn image generation primitives:
--            * artifact variants (raw / display / thumbnail) + parent linkage
--            * image_sessions  — focused per-(owner, session) state mirror
--            * image_turns     — per-turn audit + output linkage
--            * image_idempotency — dedup by client_request_id
-- Idempotent: every DDL gated by IF NOT EXISTS / ADD COLUMN IF NOT EXISTS.
-- search_path is set globally to "gateway, assistant, knowledge, public" by
-- _global/001_create_schemas.sql, so unqualified references already resolve;
-- we still qualify with `assistant.` for clarity + safety against future
-- search_path drift.
-- =============================================================================

-- (psql metacommand removed — runner is asyncpg, transaction-scoped.)

-- ---------------------------------------------------------------------------
-- 1. artifacts: variant + parent linkage + image-specific first-class fields
-- ---------------------------------------------------------------------------
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS variant            VARCHAR(16) NOT NULL DEFAULT 'raw';
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS parent_artifact_id VARCHAR(64);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS turn_id            VARCHAR(64);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS owner_scope        VARCHAR(255);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS width              INT;
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS height             INT;
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS provider           VARCHAR(64);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS model_id           VARCHAR(128);
ALTER TABLE assistant.artifacts ADD COLUMN IF NOT EXISTS prompt             TEXT;

CREATE INDEX IF NOT EXISTS idx_artifacts_variant
    ON assistant.artifacts(variant);

CREATE INDEX IF NOT EXISTS idx_artifacts_parent
    ON assistant.artifacts(parent_artifact_id)
    WHERE parent_artifact_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_artifacts_turn
    ON assistant.artifacts(turn_id)
    WHERE turn_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_artifacts_owner
    ON assistant.artifacts(owner_scope)
    WHERE owner_scope IS NOT NULL;

-- For "find variant of raw" lookup (parent_artifact_id, variant) compound
CREATE INDEX IF NOT EXISTS idx_artifacts_parent_variant
    ON assistant.artifacts(parent_artifact_id, variant)
    WHERE parent_artifact_id IS NOT NULL;

COMMENT ON COLUMN assistant.artifacts.variant IS
    'Image variant: raw (un-watermarked), display (watermarked), thumbnail (downscaled)';
COMMENT ON COLUMN assistant.artifacts.parent_artifact_id IS
    'For variant rows (display/thumbnail) points to the raw artifact_id; NULL for raw';
COMMENT ON COLUMN assistant.artifacts.owner_scope IS
    'Opaque authenticated owner scope; never derive it from unverified request fields';

-- ---------------------------------------------------------------------------
-- 2. image_sessions: focused per-(owner, session) state mirror
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant.image_sessions (
    session_id          VARCHAR(255) PRIMARY KEY,
    owner_scope         VARCHAR(255) NOT NULL,
    app_user_id         VARCHAR(255),
    app_tenant_id       VARCHAR(255),
    latest_artifact_id  VARCHAR(64),
    locked_style        VARCHAR(64),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_image_sessions_owner
    ON assistant.image_sessions(owner_scope);

CREATE INDEX IF NOT EXISTS idx_image_sessions_latest
    ON assistant.image_sessions(latest_artifact_id)
    WHERE latest_artifact_id IS NOT NULL;

COMMENT ON TABLE assistant.image_sessions IS
    'Focused state for image multi-turn editing. Holds latest artifact pointer and locked style.';

-- ---------------------------------------------------------------------------
-- 3. image_turns: per-turn audit (replaces ad-hoc image_chat_history JSONB)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant.image_turns (
    turn_id              VARCHAR(64) PRIMARY KEY,
    session_id           VARCHAR(255) NOT NULL,
    owner_scope          VARCHAR(255) NOT NULL,
    task_id              VARCHAR(64),
    prompt               TEXT,
    model_id             VARCHAR(128),
    style                VARCHAR(64),
    add_watermark        BOOLEAN NOT NULL DEFAULT TRUE,
    parent_artifact_id   VARCHAR(64),
    output_artifact_id   VARCHAR(64),
    status               VARCHAR(32) NOT NULL DEFAULT 'pending',
    error                TEXT,
    error_code           VARCHAR(64),
    client_request_id    VARCHAR(128),
    request_hash         VARCHAR(64),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at         TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_image_turns_session
    ON assistant.image_turns(session_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_image_turns_task
    ON assistant.image_turns(task_id)
    WHERE task_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_image_turns_parent
    ON assistant.image_turns(parent_artifact_id)
    WHERE parent_artifact_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_image_turns_owner
    ON assistant.image_turns(owner_scope);

COMMENT ON TABLE assistant.image_turns IS
    'Per-turn record of image generation: who-asked-what-and-got-what-back. '
    'Authoritative store for /image-task/{task_id} polling fallback.';

-- ---------------------------------------------------------------------------
-- 4. image_idempotency: dedup by client_request_id within an owner_scope
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assistant.image_idempotency (
    owner_scope        VARCHAR(255) NOT NULL,
    client_request_id  VARCHAR(128) NOT NULL,
    request_hash       VARCHAR(64)  NOT NULL,
    task_id            VARCHAR(64)  NOT NULL,
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (owner_scope, client_request_id)
);

CREATE INDEX IF NOT EXISTS idx_image_idempotency_task
    ON assistant.image_idempotency(task_id);

COMMENT ON TABLE assistant.image_idempotency IS
    'Idempotency dedup for image generation requests. Replay returns the same task_id when '
    '(owner_scope, client_request_id) match AND request_hash matches; mismatched hash → 409.';
