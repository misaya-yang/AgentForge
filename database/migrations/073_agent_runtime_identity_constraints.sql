-- 073 — Complete Agent runtime identity and session-bound checkpoint relations.
--
-- Forward-only hardening for migration 072. Existing built-in Assistant rows
-- remain valid with an entirely NULL Agent identity. Agent Preview rows bind a
-- positive draft revision; published channels bind one Agent, Version, and
-- Publication that all belong to each other.

BEGIN;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_publications_runtime_identity_key'
          AND conrelid = 'agent_publications'::regclass
    ) THEN
        ALTER TABLE agent_publications
            ADD CONSTRAINT agent_publications_runtime_identity_key
            UNIQUE (tenant_id, publication_id, agent_id, version_id);
    END IF;
END;
$$;

DO $$
DECLARE
    target_table TEXT;
    target_name TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'sessions',
        'assistant_runs',
        'assistant_run_checkpoints',
        'agent_traces'
    ]
    LOOP
        target_name := target_table || '_agent_runtime_shape_check';
        EXECUTE format(
            'ALTER TABLE %I DROP CONSTRAINT IF EXISTS %I',
            target_table,
            target_name
        );
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I CHECK ('
            || '(agent_id IS NULL AND agent_version_id IS NULL '
            || 'AND agent_draft_revision IS NULL AND publication_id IS NULL '
            || 'AND channel IS NULL AND runtime_fingerprint IS NULL '
            || 'AND agent_spec_hash IS NULL) OR '
            || '(agent_id IS NOT NULL '
            || 'AND runtime_fingerprint IS NOT NULL '
            || 'AND agent_spec_hash IS NOT NULL AND ('
            || '(channel = ''preview'' AND agent_draft_revision >= 1 '
            || 'AND agent_version_id IS NULL AND publication_id IS NULL) OR '
            || '(channel IN (''hosted'', ''embed'', ''api'') '
            || 'AND agent_draft_revision IS NULL '
            || 'AND agent_version_id IS NOT NULL '
            || 'AND publication_id IS NOT NULL))))',
            target_table,
            target_name
        );

        target_name := target_table || '_runtime_agent_version_fk';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = target_name AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I '
                || 'FOREIGN KEY (tenant_id, agent_id, agent_version_id) '
                || 'REFERENCES agent_versions('
                || 'tenant_id, agent_id, agent_version_id) ON DELETE RESTRICT',
                target_table,
                target_name
            );
        END IF;

        target_name := target_table || '_runtime_publication_identity_fk';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = target_name AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I '
                || 'FOREIGN KEY ('
                || 'tenant_id, publication_id, agent_id, agent_version_id) '
                || 'REFERENCES agent_publications('
                || 'tenant_id, publication_id, agent_id, version_id) '
                || 'ON DELETE RESTRICT',
                target_table,
                target_name
            );
        END IF;
    END LOOP;
END;
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_runs_resume_scope_key'
          AND conrelid = 'assistant_runs'::regclass
    ) THEN
        ALTER TABLE assistant_runs
            ADD CONSTRAINT assistant_runs_resume_scope_key
            UNIQUE (run_id, tenant_id, user_id, session_id);
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_checkpoints_resume_scope_fk'
          AND conrelid = 'assistant_run_checkpoints'::regclass
    ) THEN
        ALTER TABLE assistant_run_checkpoints
            ADD CONSTRAINT assistant_checkpoints_resume_scope_fk
            FOREIGN KEY (run_id, tenant_id, user_id, session_id)
            REFERENCES assistant_runs(run_id, tenant_id, user_id, session_id)
            ON DELETE CASCADE;
    END IF;
END;
$$;

COMMIT;
