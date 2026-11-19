//! Fenced-attempt queries copied from the canonical Python state machine.
pub const _SELECT_LATEST_ATTEMPT: &str = r###"
SELECT attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
       phase, checkpoint, lease_owner, fence_generation, state, started_at
FROM public.platform_schema_change_attempts
WHERE baseline_id = $1 AND sequence = $2
ORDER BY started_at DESC, attempt_id DESC
LIMIT 1
"###;
pub const _CLAIM_ATTEMPT: &str = r###"
UPDATE public.platform_schema_change_attempts
SET lease_owner = $2, fence_generation = fence_generation + 1,
    state = 'running', phase = $4,
    started_at = now(), finished_at = NULL, error_code = NULL
WHERE attempt_id = $1 AND fence_generation = $3
  AND state IN ('resumable', 'failed')
RETURNING fence_generation
"###;
pub const _TRANSITION_EXPIRED_ATTEMPT: &str = r###"
UPDATE public.platform_schema_change_attempts
SET state = $4, finished_at = now(), error_code = $5
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = 'running'
RETURNING fence_generation
"###;
pub const _UPDATE_ATTEMPT_CHECKPOINT_FENCED: &str = r###"
UPDATE public.platform_schema_change_attempts
SET checkpoint = $4, fence_generation = fence_generation + 1, started_at = now()
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = 'running'
RETURNING fence_generation
"###;
pub const _UPDATE_ATTEMPT_TERMINAL_FENCED: &str = r###"
UPDATE public.platform_schema_change_attempts
SET state = $4, finished_at = now(), error_code = $5,
    fence_generation = fence_generation + 1
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = 'running'
RETURNING fence_generation
"###;
