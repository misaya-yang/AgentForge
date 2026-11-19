-- Forward-only: an eval worker owns one expiring claim, independent of attempts.
ALTER TABLE gateway.agent_trace_outbox
    ADD COLUMN owner_id text,
    ADD COLUMN claim_token uuid,
    ADD COLUMN lease_until timestamptz,
    ADD COLUMN heartbeat_at timestamptz;

-- Migration requires old workers to be quiescent. Preserve history and make
-- interrupted legacy jobs eligible for reconciliation by the new owner.
UPDATE gateway.agent_trace_outbox
SET owner_id = 'legacy-migration', claim_token = gen_random_uuid(),
    lease_until = NOW(), heartbeat_at = NOW()
WHERE status = 'running';

ALTER TABLE gateway.agent_trace_outbox ADD CONSTRAINT eval_outbox_running_lease
    CHECK (status <> 'running' OR
           (owner_id IS NOT NULL AND claim_token IS NOT NULL AND lease_until IS NOT NULL));
CREATE INDEX eval_outbox_expired_lease
    ON gateway.agent_trace_outbox(lease_until) WHERE status = 'running';

ALTER TABLE gateway.eval_experiment_run_cases
    ADD COLUMN runtime_handle jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN dispatch_state text NOT NULL DEFAULT 'not_started',
    ADD CONSTRAINT eval_case_handle_object CHECK (jsonb_typeof(runtime_handle) = 'object'),
    ADD CONSTRAINT eval_case_dispatch_state CHECK
        (dispatch_state IN ('not_started', 'dispatching', 'accepted', 'reconcile_required'));

-- A legacy running case may already have produced effects without a handle.
-- Preserve it for operator reconciliation; never treat it as undispatched.
UPDATE gateway.eval_experiment_run_cases
SET dispatch_state = 'reconcile_required'
WHERE status = 'running';
