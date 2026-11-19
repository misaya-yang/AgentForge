-- Migration: 061_trace_eval_sota_core.sql
-- Goal:
-- 1) Add the ATE-03 Eval core tables for datasets, examples, evaluators,
--    experiments, experiment runs, and durable async jobs.
-- 2) Extend trace scores with target/evaluator/version metadata while keeping
--    the existing score API compatible.
-- 3) Preserve the current agent_traces schema and add only additive columns.

BEGIN;

ALTER TABLE agent_traces
    ADD COLUMN IF NOT EXISTS source_adapter VARCHAR(64),
    ADD COLUMN IF NOT EXISTS thread_id VARCHAR(100),
    ADD COLUMN IF NOT EXISTS metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS privacy JSONB NOT NULL DEFAULT '{}'::jsonb;

UPDATE agent_traces
SET thread_id = COALESCE(thread_id, session_id)
WHERE thread_id IS NULL AND session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_thread
    ON agent_traces(tenant_id, thread_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_traces_source_adapter
    ON agent_traces(source_adapter, created_at DESC);

ALTER TABLE agent_trace_scores
    ADD COLUMN IF NOT EXISTS target_type VARCHAR(32) NOT NULL DEFAULT 'trace',
    ADD COLUMN IF NOT EXISTS target_id VARCHAR(128),
    ADD COLUMN IF NOT EXISTS evaluator_id UUID,
    ADD COLUMN IF NOT EXISTS evaluator_name VARCHAR(128),
    ADD COLUMN IF NOT EXISTS score_source VARCHAR(32) NOT NULL DEFAULT 'human',
    ADD COLUMN IF NOT EXISTS confidence DOUBLE PRECISION;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_scores_target_type'
    ) THEN
        ALTER TABLE agent_trace_scores
        ADD CONSTRAINT chk_agent_trace_scores_target_type
        CHECK (target_type IN ('trace', 'span', 'thread', 'dataset_run', 'example'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_scores_source'
    ) THEN
        ALTER TABLE agent_trace_scores
        ADD CONSTRAINT chk_agent_trace_scores_source
        CHECK (score_source IN ('human', 'llm', 'rule', 'system', 'imported'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_trace_scores_target
    ON agent_trace_scores(target_type, target_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_trace_scores_evaluator
    ON agent_trace_scores(evaluator_id, created_at DESC);

CREATE TABLE IF NOT EXISTS eval_datasets (
    dataset_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    name VARCHAR(160) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    version VARCHAR(64) NOT NULL DEFAULT 'v1',
    schema JSONB NOT NULL DEFAULT '{}'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, name, version)
);

CREATE TABLE IF NOT EXISTS eval_examples (
    example_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dataset_id UUID NOT NULL REFERENCES eval_datasets(dataset_id) ON DELETE CASCADE,
    tenant_id VARCHAR(64) NOT NULL,
    split VARCHAR(32) NOT NULL DEFAULT 'regression',
    input JSONB NOT NULL DEFAULT '{}'::jsonb,
    expected_output JSONB NOT NULL DEFAULT '{}'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_trace_id UUID REFERENCES agent_traces(trace_id) ON DELETE SET NULL,
    source_span_id UUID REFERENCES agent_trace_spans(span_id) ON DELETE SET NULL,
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_eval_examples_dataset_split
    ON eval_examples(dataset_id, split, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_eval_examples_source_trace
    ON eval_examples(source_trace_id);

CREATE TABLE IF NOT EXISTS eval_evaluators (
    evaluator_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    name VARCHAR(128) NOT NULL,
    evaluator_type VARCHAR(32) NOT NULL DEFAULT 'human',
    rubric TEXT NOT NULL DEFAULT '',
    version VARCHAR(64) NOT NULL DEFAULT 'v1',
    sampling_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    filter_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, name, version)
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_eval_evaluators_type'
    ) THEN
        ALTER TABLE eval_evaluators
        ADD CONSTRAINT chk_eval_evaluators_type
        CHECK (evaluator_type IN ('human', 'rule', 'llm', 'composite'));
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS eval_experiments (
    experiment_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    dataset_id UUID REFERENCES eval_datasets(dataset_id) ON DELETE SET NULL,
    name VARCHAR(160) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    target_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_eval_experiments_tenant_created
    ON eval_experiments(tenant_id, created_at DESC);

CREATE TABLE IF NOT EXISTS eval_experiment_runs (
    run_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    experiment_id UUID REFERENCES eval_experiments(experiment_id) ON DELETE CASCADE,
    tenant_id VARCHAR(64) NOT NULL,
    evaluator_id UUID REFERENCES eval_evaluators(evaluator_id) ON DELETE SET NULL,
    dataset_id UUID REFERENCES eval_datasets(dataset_id) ON DELETE SET NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    target_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    score_summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_message TEXT,
    created_by VARCHAR(64) NOT NULL,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_eval_experiment_runs_status'
    ) THEN
        ALTER TABLE eval_experiment_runs
        ADD CONSTRAINT chk_eval_experiment_runs_status
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_eval_experiment_runs_experiment_created
    ON eval_experiment_runs(experiment_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_eval_experiment_runs_tenant_status
    ON eval_experiment_runs(tenant_id, status, created_at DESC);

CREATE TABLE IF NOT EXISTS agent_trace_outbox (
    job_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    job_type VARCHAR(96) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_outbox_status'
    ) THEN
        ALTER TABLE agent_trace_outbox
        ADD CONSTRAINT chk_agent_trace_outbox_status
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_trace_outbox_tenant_status
    ON agent_trace_outbox(tenant_id, status, available_at, created_at);

CREATE OR REPLACE FUNCTION update_eval_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_eval_datasets_timestamp ON eval_datasets;
CREATE TRIGGER update_eval_datasets_timestamp
    BEFORE UPDATE ON eval_datasets
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

DROP TRIGGER IF EXISTS update_eval_evaluators_timestamp ON eval_evaluators;
CREATE TRIGGER update_eval_evaluators_timestamp
    BEFORE UPDATE ON eval_evaluators
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

DROP TRIGGER IF EXISTS update_eval_experiments_timestamp ON eval_experiments;
CREATE TRIGGER update_eval_experiments_timestamp
    BEFORE UPDATE ON eval_experiments
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

DROP TRIGGER IF EXISTS update_eval_experiment_runs_timestamp ON eval_experiment_runs;
CREATE TRIGGER update_eval_experiment_runs_timestamp
    BEFORE UPDATE ON eval_experiment_runs
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

DROP TRIGGER IF EXISTS update_agent_trace_outbox_timestamp ON agent_trace_outbox;
CREATE TRIGGER update_agent_trace_outbox_timestamp
    BEFORE UPDATE ON agent_trace_outbox
    FOR EACH ROW
    EXECUTE FUNCTION update_eval_timestamp();

COMMIT;
