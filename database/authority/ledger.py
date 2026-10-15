"""Ledger tables for the single migration authority.

* ``platform_schema_baselines`` — one row per adopted/fresh-installed
  baseline.  The adoption marker.
* ``platform_schema_changes`` — successful changes only.  Primary key
  ``(baseline_id, sequence)``; the full SHA-256 is immutable forever.
* ``platform_schema_change_attempts`` — used ONLY by explicit
  non-transactional changes.  Transactional changes never write a started
  state because their DDL and success ledger row commit atomically.

All three tables live in ``public`` and are written exclusively by sessions
holding the migration advisory lock.  ``status``/``verify`` surfaces open
read-only connections and can therefore never touch them.
"""

from __future__ import annotations

from .constants import ATTEMPTS_TABLE, BASELINES_TABLE, CHANGES_TABLE

LEDGER_DDL = f"""
CREATE TABLE IF NOT EXISTS public.{BASELINES_TABLE} (
    baseline_id             TEXT PRIMARY KEY,
    manifest_sha256         TEXT NOT NULL,
    structural_sha256       TEXT NOT NULL,
    acl_sha256              TEXT NOT NULL,
    extensions_sha256       TEXT NOT NULL,
    reference_data_sha256   TEXT NOT NULL,
    source_git_sha          TEXT NOT NULL,
    adopted_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS public.{CHANGES_TABLE} (
    baseline_id     TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    name            TEXT NOT NULL,
    checksum_sha256 TEXT NOT NULL,
    applied_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    duration_ms     INTEGER NOT NULL,
    runner_digest   TEXT NOT NULL,
    PRIMARY KEY (baseline_id, sequence)
);

CREATE TABLE IF NOT EXISTS public.{ATTEMPTS_TABLE} (
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
"""

# An attempt is resumable only while its lease is held by the same runner and
# its state machine has not reached a terminal verdict.
ATTEMPT_STATE_RUNNING = "running"
ATTEMPT_STATE_RESUMABLE = "resumable"
ATTEMPT_STATE_FAILED = "failed"
ATTEMPT_STATE_SUCCEEDED = "succeeded"
TERMINAL_ATTEMPT_STATES = frozenset({ATTEMPT_STATE_FAILED, ATTEMPT_STATE_SUCCEEDED})

INSERT_BASELINE_MARKER = f"""
INSERT INTO public.{BASELINES_TABLE} (
    baseline_id, manifest_sha256, structural_sha256, acl_sha256,
    extensions_sha256, reference_data_sha256, source_git_sha
)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (baseline_id) DO NOTHING
"""

INSERT_CHANGE_SUCCESS = f"""
INSERT INTO public.{CHANGES_TABLE} (
    baseline_id, sequence, name, checksum_sha256, applied_at, duration_ms, runner_digest
)
VALUES ($1, $2, $3, $4, now(), $5, $6)
"""

SELECT_APPLIED_CHANGES = f"""
SELECT sequence, checksum_sha256
FROM public.{CHANGES_TABLE}
WHERE baseline_id = $1
ORDER BY sequence
"""

SELECT_BASELINE = f"""
SELECT baseline_id, manifest_sha256, structural_sha256, acl_sha256,
       extensions_sha256, reference_data_sha256, source_git_sha, adopted_at
FROM public.{BASELINES_TABLE}
ORDER BY baseline_id
"""

INSERT_ATTEMPT = f"""
INSERT INTO public.{ATTEMPTS_TABLE} (
    attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
    phase, checkpoint, lease_owner, fence_generation, state
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '{ATTEMPT_STATE_RUNNING}')
"""

UPDATE_ATTEMPT_CHECKPOINT = f"""
UPDATE public.{ATTEMPTS_TABLE}
SET checkpoint = $3, fence_generation = fence_generation + 1
WHERE attempt_id = $1 AND lease_owner = $2
"""

UPDATE_ATTEMPT_TERMINAL = f"""
UPDATE public.{ATTEMPTS_TABLE}
SET state = $3, finished_at = now(), error_code = $4
WHERE attempt_id = $1 AND lease_owner = $2
"""

SELECT_OPEN_ATTEMPTS = f"""
SELECT attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
       phase, checkpoint, lease_owner, fence_generation, state, started_at
FROM public.{ATTEMPTS_TABLE}
WHERE baseline_id = $1 AND sequence = $2 AND state NOT IN ('{ATTEMPT_STATE_FAILED}', '{ATTEMPT_STATE_SUCCEEDED}')
ORDER BY started_at DESC
"""
