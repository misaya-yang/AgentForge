-- 067 — assistant run checkpoints for durable resume preparation.
--
-- Additive only. Checkpoints store bounded state summaries and hashes, not raw
-- prompts, unbounded messages, credentials, or full tool arguments.

CREATE TABLE IF NOT EXISTS assistant_run_checkpoints (
    checkpoint_id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES assistant_runs(run_id) ON DELETE CASCADE,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    phase VARCHAR(64) NOT NULL,
    iteration INTEGER NOT NULL DEFAULT 0,
    message_state_hash VARCHAR(64) NOT NULL,
    pending_tool JSONB NOT NULL DEFAULT '{}'::jsonb,
    approval_id UUID,
    idempotency_keys JSONB NOT NULL DEFAULT '{}'::jsonb,
    resume_payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_run_checkpoints_latest
    ON assistant_run_checkpoints(tenant_id, user_id, run_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_assistant_run_checkpoints_status
    ON assistant_run_checkpoints(tenant_id, status, created_at DESC);

COMMENT ON TABLE assistant_run_checkpoints IS
    'Additive assistant resume checkpoints. Payloads are bounded summaries/hashes, not raw prompts or full tool args.';
