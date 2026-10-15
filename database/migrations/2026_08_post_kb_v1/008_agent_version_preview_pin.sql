-- R3 AG-03: Preview sessions may pin either a Draft revision or an immutable
-- Agent Version. Both forms remain private to the builder and never bind a
-- Publication. Existing builtin and published shapes are unchanged.
DO $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT namespace.nspname AS schema_name,
               relation.relname AS table_name,
               constraint_row.conname AS constraint_name
        FROM pg_constraint AS constraint_row
        JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
        JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
        WHERE namespace.nspname IN ('assistant', 'gateway')
          AND constraint_row.conname IN (
              'sessions_agent_runtime_shape_check',
              'assistant_runs_agent_runtime_shape_check',
              'assistant_run_checkpoints_agent_runtime_shape_check',
              'agent_traces_agent_runtime_shape_check'
          )
    LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I DROP CONSTRAINT %I',
            target.schema_name, target.table_name, target.constraint_name
        );
        EXECUTE format(
            'ALTER TABLE %I.%I ADD CONSTRAINT %I CHECK ('
            || '(agent_id IS NULL AND agent_version_id IS NULL '
            || 'AND agent_draft_revision IS NULL AND publication_id IS NULL '
            || 'AND channel IS NULL AND runtime_fingerprint IS NULL '
            || 'AND agent_spec_hash IS NULL) OR '
            || '(agent_id IS NOT NULL AND runtime_fingerprint IS NOT NULL '
            || 'AND agent_spec_hash IS NOT NULL AND ('
            || '(channel = ''preview'' AND publication_id IS NULL AND ('
            || '(agent_draft_revision >= 1 AND agent_version_id IS NULL) OR '
            || '(agent_draft_revision IS NULL AND agent_version_id IS NOT NULL))) OR '
            || '(channel IN (''hosted'', ''embed'', ''api'') '
            || 'AND agent_draft_revision IS NULL AND agent_version_id IS NOT NULL '
            || 'AND publication_id IS NOT NULL))))',
            target.schema_name, target.table_name, target.constraint_name
        );
    END LOOP;
END;
$$;
