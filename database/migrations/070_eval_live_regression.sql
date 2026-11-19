-- Migration: 070_eval_live_regression.sql
-- Goal: add immutable live-eval run cases and explicit baseline lifecycle.

BEGIN;

ALTER TABLE eval_experiment_runs
    ADD COLUMN IF NOT EXISTS run_mode VARCHAR(32) NOT NULL DEFAULT 'rescore_trace',
    ADD COLUMN IF NOT EXISTS repetitions INTEGER NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS baseline_run_id UUID REFERENCES eval_experiment_runs(run_id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS dataset_manifest_hash VARCHAR(64),
    ADD COLUMN IF NOT EXISTS evaluator_suite_hash VARCHAR(64),
    ADD COLUMN IF NOT EXISTS candidate_fingerprint JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS execution_config JSONB NOT NULL DEFAULT '{}'::jsonb;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_eval_experiment_runs_mode'
    ) THEN
        ALTER TABLE eval_experiment_runs
        ADD CONSTRAINT chk_eval_experiment_runs_mode
        CHECK (run_mode IN ('rescore_trace', 'live_candidate'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_eval_experiment_runs_repetitions'
    ) THEN
        ALTER TABLE eval_experiment_runs
        ADD CONSTRAINT chk_eval_experiment_runs_repetitions
        CHECK (repetitions BETWEEN 1 AND 10);
    END IF;
END $$;

ALTER TABLE eval_experiments
    ADD COLUMN IF NOT EXISTS baseline_run_id UUID REFERENCES eval_experiment_runs(run_id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS baseline_promoted_by VARCHAR(64),
    ADD COLUMN IF NOT EXISTS baseline_promoted_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS eval_experiment_run_cases (
    run_case_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id UUID NOT NULL REFERENCES eval_experiment_runs(run_id) ON DELETE CASCADE,
    tenant_id VARCHAR(64) NOT NULL,
    case_id VARCHAR(160) NOT NULL,
    example_id UUID REFERENCES eval_examples(example_id) ON DELETE SET NULL,
    trial_index INTEGER NOT NULL DEFAULT 1,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    input JSONB NOT NULL DEFAULT '{}'::jsonb,
    expected_output JSONB NOT NULL DEFAULT '{}'::jsonb,
    expected_trajectory JSONB NOT NULL DEFAULT '{}'::jsonb,
    assertions JSONB NOT NULL DEFAULT '[]'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    candidate_trace_id UUID REFERENCES agent_traces(trace_id) ON DELETE SET NULL,
    observed_metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (run_id, case_id, trial_index),
    CHECK (trial_index BETWEEN 1 AND 10),
    CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'skipped'))
);

CREATE INDEX IF NOT EXISTS idx_eval_run_cases_run_status
    ON eval_experiment_run_cases(run_id, status, case_id, trial_index);
CREATE INDEX IF NOT EXISTS idx_eval_run_cases_tenant_trace
    ON eval_experiment_run_cases(tenant_id, candidate_trace_id);

CREATE TABLE IF NOT EXISTS eval_baseline_promotions (
    promotion_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    experiment_id UUID NOT NULL REFERENCES eval_experiments(experiment_id) ON DELETE CASCADE,
    previous_baseline_run_id UUID REFERENCES eval_experiment_runs(run_id) ON DELETE SET NULL,
    baseline_run_id UUID NOT NULL REFERENCES eval_experiment_runs(run_id) ON DELETE RESTRICT,
    promoted_by VARCHAR(64) NOT NULL,
    promoted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_eval_baseline_promotions_experiment
    ON eval_baseline_promotions(tenant_id, experiment_id, promoted_at DESC);

DROP TRIGGER IF EXISTS update_eval_experiment_run_cases_timestamp
    ON eval_experiment_run_cases;
CREATE TRIGGER update_eval_experiment_run_cases_timestamp
    BEFORE UPDATE ON eval_experiment_run_cases
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

COMMIT;
