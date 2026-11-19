//! SQL copied from the retained authority ledger, without schema changes.
pub const LEDGER_DDL: &str = r###"
CREATE TABLE IF NOT EXISTS public.platform_schema_baselines (
    baseline_id             TEXT PRIMARY KEY,
    manifest_sha256         TEXT NOT NULL,
    structural_sha256       TEXT NOT NULL,
    acl_sha256              TEXT NOT NULL,
    extensions_sha256       TEXT NOT NULL,
    reference_data_sha256   TEXT NOT NULL,
    source_git_sha          TEXT NOT NULL,
    adopted_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS public.platform_schema_changes (
    baseline_id     TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    name            TEXT NOT NULL,
    checksum_sha256 TEXT NOT NULL,
    applied_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    duration_ms     INTEGER NOT NULL,
    runner_digest   TEXT NOT NULL,
    PRIMARY KEY (baseline_id, sequence)
);

CREATE TABLE IF NOT EXISTS public.platform_schema_change_attempts (
    attempt_id      TEXT PRIMARY KEY,
    baseline_id     TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    checksum_sha256 TEXT NOT NULL,
    runner_digest   TEXT NOT NULL,
    phase           TEXT NOT NULL,
    checkpoint      TEXT NOT NULL DEFAULT '',
    lease_owner     TEXT NOT NULL,
    fence_generation INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    error_code      TEXT
);
"###;
pub const ATTEMPT_STATE_RUNNING: &str = r###"running"###;
pub const ATTEMPT_STATE_RESUMABLE: &str = r###"resumable"###;
pub const ATTEMPT_STATE_FAILED: &str = r###"failed"###;
pub const ATTEMPT_STATE_SUCCEEDED: &str = r###"succeeded"###;
pub const INSERT_BASELINE_MARKER: &str = r###"
INSERT INTO public.platform_schema_baselines (
    baseline_id, manifest_sha256, structural_sha256, acl_sha256,
    extensions_sha256, reference_data_sha256, source_git_sha
)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (baseline_id) DO NOTHING
"###;
pub const INSERT_CHANGE_SUCCESS: &str = r###"
INSERT INTO public.platform_schema_changes (
    baseline_id, sequence, name, checksum_sha256, applied_at, duration_ms, runner_digest
)
VALUES ($1, $2, $3, $4, now(), $5, $6)
"###;
pub const SELECT_APPLIED_CHANGES: &str = r###"
SELECT sequence, checksum_sha256
FROM public.platform_schema_changes
WHERE baseline_id = $1
ORDER BY sequence
"###;
pub const SELECT_BASELINE: &str = r###"
SELECT baseline_id, manifest_sha256, structural_sha256, acl_sha256,
       extensions_sha256, reference_data_sha256, source_git_sha, adopted_at
FROM public.platform_schema_baselines
ORDER BY baseline_id
"###;
pub const INSERT_ATTEMPT: &str = r###"
INSERT INTO public.platform_schema_change_attempts (
    attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
    phase, checkpoint, lease_owner, fence_generation, state
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'running')
"###;
pub const UPDATE_ATTEMPT_CHECKPOINT: &str = r###"
UPDATE public.platform_schema_change_attempts
SET checkpoint = $3, fence_generation = fence_generation + 1
WHERE attempt_id = $1 AND lease_owner = $2
"###;
pub const UPDATE_ATTEMPT_TERMINAL: &str = r###"
UPDATE public.platform_schema_change_attempts
SET state = $3, finished_at = now(), error_code = $4
WHERE attempt_id = $1 AND lease_owner = $2
"###;
pub const SELECT_OPEN_ATTEMPTS: &str = r###"
SELECT attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
       phase, checkpoint, lease_owner, fence_generation, state, started_at
FROM public.platform_schema_change_attempts
WHERE baseline_id = $1 AND sequence = $2 AND state NOT IN ('failed', 'succeeded')
ORDER BY started_at DESC
"###;

pub const BASELINES_TABLE: &str = "platform_schema_baselines";
pub const CHANGES_TABLE: &str = "platform_schema_changes";
pub const ATTEMPTS_TABLE: &str = "platform_schema_change_attempts";
