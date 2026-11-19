-- Migration: 078_agent_release_lifecycle_hardening.sql
-- Goal: make Agent release evaluation lifecycle durable/cancellable, bind
--       selected Eval Dataset content, and enforce tenant-composite evidence.

BEGIN;

CREATE UNIQUE INDEX IF NOT EXISTS idx_eval_datasets_tenant_dataset_unique
    ON eval_datasets(tenant_id, dataset_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_eval_experiment_runs_tenant_run_unique
    ON eval_experiment_runs(tenant_id, run_id);

ALTER TABLE agent_release_evaluations
    ADD COLUMN IF NOT EXISTS dataset_version VARCHAR(64),
    ADD COLUMN IF NOT EXISTS dataset_manifest_hash CHAR(64),
    ADD COLUMN IF NOT EXISTS evaluation_identity_hash CHAR(64),
    ADD COLUMN IF NOT EXISTS started_at TIMESTAMPTZ;

ALTER TABLE agent_release_evaluations
    ALTER COLUMN completed_at DROP NOT NULL,
    ALTER COLUMN completed_at DROP DEFAULT;

ALTER TABLE agent_release_evaluations
    DROP CONSTRAINT IF EXISTS agent_release_evaluations_status_check;
ALTER TABLE agent_release_evaluations
    ADD CONSTRAINT agent_release_evaluations_status_check
    CHECK (status IN ('queued', 'running', 'passed', 'failed', 'cancelled'));

ALTER TABLE agent_release_evaluations
    DROP CONSTRAINT IF EXISTS agent_release_evaluations_dataset_manifest_hash_check;
ALTER TABLE agent_release_evaluations
    ADD CONSTRAINT agent_release_evaluations_dataset_manifest_hash_check
    CHECK (
        dataset_manifest_hash IS NULL
        OR dataset_manifest_hash ~ '^[0-9a-f]{64}$'
    );

ALTER TABLE agent_release_evaluations
    DROP CONSTRAINT IF EXISTS agent_release_evaluations_identity_hash_check;
ALTER TABLE agent_release_evaluations
    ADD CONSTRAINT agent_release_evaluations_identity_hash_check
    CHECK (
        evaluation_identity_hash IS NULL
        OR evaluation_identity_hash ~ '^[0-9a-f]{64}$'
    );

ALTER TABLE agent_release_evaluations
    DROP CONSTRAINT IF EXISTS agent_release_evaluations_dataset_fk,
    DROP CONSTRAINT IF EXISTS agent_release_evaluations_run_fk;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_release_evaluations_dataset_tenant_fk'
          AND conrelid = 'agent_release_evaluations'::regclass
    ) THEN
        ALTER TABLE agent_release_evaluations
            ADD CONSTRAINT agent_release_evaluations_dataset_tenant_fk
            FOREIGN KEY (tenant_id, dataset_id)
            REFERENCES eval_datasets(tenant_id, dataset_id)
            ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_release_evaluations_run_tenant_fk'
          AND conrelid = 'agent_release_evaluations'::regclass
    ) THEN
        ALTER TABLE agent_release_evaluations
            ADD CONSTRAINT agent_release_evaluations_run_tenant_fk
            FOREIGN KEY (tenant_id, experiment_run_id)
            REFERENCES eval_experiment_runs(tenant_id, run_id)
            ON DELETE RESTRICT;
    END IF;
END;
$$;

CREATE OR REPLACE FUNCTION agent_studio_guard_release_evaluation_lifecycle()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'AGENT_RELEASE_EVIDENCE_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;

    IF OLD.status IN ('passed', 'failed', 'cancelled') THEN
        RAISE EXCEPTION 'AGENT_RELEASE_EVIDENCE_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;

    IF (
        to_jsonb(NEW) - ARRAY[
            'status',
            'validation_snapshot',
            'gate_snapshot',
            'started_at',
            'completed_at'
        ]::TEXT[]
    ) IS DISTINCT FROM (
        to_jsonb(OLD) - ARRAY[
            'status',
            'validation_snapshot',
            'gate_snapshot',
            'started_at',
            'completed_at'
        ]::TEXT[]
    ) THEN
        RAISE EXCEPTION 'AGENT_RELEASE_EVIDENCE_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;

    IF NOT (
        (OLD.status = 'queued' AND NEW.status IN ('running', 'cancelled'))
        OR (OLD.status = 'running' AND NEW.status IN ('passed', 'failed', 'cancelled'))
    ) THEN
        RAISE EXCEPTION 'AGENT_RELEASE_LIFECYCLE_INVALID'
            USING ERRCODE = '55000';
    END IF;

    IF NEW.status = 'running' AND NEW.started_at IS NULL THEN
        RAISE EXCEPTION 'AGENT_RELEASE_LIFECYCLE_INVALID'
            USING ERRCODE = '55000';
    END IF;
    IF NEW.status IN ('passed', 'failed', 'cancelled') AND NEW.completed_at IS NULL THEN
        RAISE EXCEPTION 'AGENT_RELEASE_LIFECYCLE_INVALID'
            USING ERRCODE = '55000';
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_release_evaluations_immutable
    ON agent_release_evaluations;
CREATE TRIGGER agent_release_evaluations_immutable
    BEFORE UPDATE OR DELETE ON agent_release_evaluations
    FOR EACH ROW EXECUTE FUNCTION agent_studio_guard_release_evaluation_lifecycle();

COMMIT;
