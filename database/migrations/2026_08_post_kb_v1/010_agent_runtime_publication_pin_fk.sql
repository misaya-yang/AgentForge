-- R3 AG-08: runtime rows pin the Version selected when a session starts.
-- A Publication's current version is mutable, so it cannot be the referenced
-- key for those rows. Keep immutable Publication/Agent and Agent/Version FKs.
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
              'sessions_runtime_publication_identity_fk',
              'assistant_runs_runtime_publication_identity_fk',
              'assistant_run_checkpoints_runtime_publication_identity_fk',
              'agent_traces_runtime_publication_identity_fk'
          )
    LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I DROP CONSTRAINT %I',
            target.schema_name, target.table_name, target.constraint_name
        );
        EXECUTE format(
            'ALTER TABLE %I.%I ADD CONSTRAINT %I '
            || 'FOREIGN KEY (tenant_id, publication_id, agent_id) '
            || 'REFERENCES gateway.agent_publications'
            || '(tenant_id, publication_id, agent_id) ON DELETE RESTRICT',
            target.schema_name, target.table_name,
            target.table_name || '_runtime_publication_agent_fk'
        );
    END LOOP;
END;
$$;
