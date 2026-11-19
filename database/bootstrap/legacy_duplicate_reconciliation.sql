-- Reconcile the proven Phase-6 split-brain layout before immutable baseline cutover.
--
-- A historical deployment could run later legacy migrations after the Phase-6
-- table move with a gateway-first search_path. That produced a second physical
-- copy of several Assistant/Knowledge tables. This repair is deliberately
-- narrow and fail-closed:
--   * rows are copied only when primary/logical keys do not overlap;
--   * only the five observed data-bearing legacy copies may be non-empty;
--   * every superseded physical table is retained in platform_legacy;
--   * the immutable cutover fingerprint remains the final proof of shape.

DO $reconcile_data$
DECLARE
    source_rows BIGINT;
    inserted_rows BIGINT;
BEGIN
    IF to_regclass('gateway.sessions') IS NOT NULL
       AND to_regclass('assistant.sessions') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM gateway.sessions AS source
            JOIN assistant.sessions AS target
              ON source.session_id = target.session_id
              OR (source.tenant_id, source.user_id, source.session_id)
                 IS NOT DISTINCT FROM
                 (target.tenant_id, target.user_id, target.session_id)
        ) THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: sessions overlap';
        END IF;
        SELECT count(*) INTO source_rows FROM gateway.sessions;
        INSERT INTO assistant.sessions (
            session_id, service_id, user_id, tenant_id, state, history,
            metadata, config, status, expires_at, created_at, updated_at
        )
        SELECT session_id, service_id, user_id, tenant_id, state, history,
               metadata, config, status, expires_at, created_at, updated_at
        FROM gateway.sessions;
        GET DIAGNOSTICS inserted_rows = ROW_COUNT;
        IF inserted_rows <> source_rows THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: sessions copied %/%',
                inserted_rows, source_rows;
        END IF;
    END IF;

    IF to_regclass('gateway.session_memory') IS NOT NULL
       AND to_regclass('assistant.session_memory') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM gateway.session_memory AS source
            JOIN assistant.session_memory AS target
              ON source.id = target.id
              OR (source.tenant_id, source.session_id, source.key) =
                 (target.tenant_id, target.session_id, target.key)
        ) THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: session_memory overlap';
        END IF;
        SELECT count(*) INTO source_rows FROM gateway.session_memory;
        INSERT INTO assistant.session_memory (
            id, tenant_id, session_id, key, value, metadata, created_at,
            updated_at, namespace, expires_at, sensitivity, source
        )
        SELECT id, tenant_id, session_id, key, value, metadata, created_at,
               updated_at, namespace, expires_at, sensitivity, source
        FROM gateway.session_memory;
        GET DIAGNOSTICS inserted_rows = ROW_COUNT;
        IF inserted_rows <> source_rows THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: session_memory copied %/%',
                inserted_rows, source_rows;
        END IF;
    END IF;

    IF to_regclass('gateway.user_memory') IS NOT NULL
       AND to_regclass('assistant.user_memory') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM gateway.user_memory AS source
            JOIN assistant.user_memory AS target
              ON source.id = target.id
              OR (source.tenant_id, source.user_id, source.key) =
                 (target.tenant_id, target.user_id, target.key)
        ) THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: user_memory overlap';
        END IF;
        SELECT count(*) INTO source_rows FROM gateway.user_memory;
        INSERT INTO assistant.user_memory (
            id, tenant_id, user_id, key, value, metadata, access_count,
            last_accessed_at, created_at, updated_at, namespace, expires_at,
            sensitivity, source
        )
        SELECT id, tenant_id, user_id, key, value, metadata, access_count,
               last_accessed_at, created_at, updated_at, namespace, expires_at,
               sensitivity, source
        FROM gateway.user_memory;
        GET DIAGNOSTICS inserted_rows = ROW_COUNT;
        IF inserted_rows <> source_rows THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: user_memory copied %/%',
                inserted_rows, source_rows;
        END IF;
    END IF;

    IF to_regclass('gateway.assistant_runs') IS NOT NULL
       AND to_regclass('assistant.assistant_runs') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM gateway.assistant_runs AS source
            JOIN assistant.assistant_runs AS target USING (run_id)
        ) THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: assistant_runs overlap';
        END IF;
        EXECUTE 'ALTER TABLE assistant.assistant_runs '
            'ADD COLUMN IF NOT EXISTS harness_thread_id UUID, '
            'ADD COLUMN IF NOT EXISTS harness_turn_id VARCHAR(255), '
            'ADD COLUMN IF NOT EXISTS runtime_snapshot_id UUID, '
            'ADD COLUMN IF NOT EXISTS kernel_revision VARCHAR(100), '
            'ADD COLUMN IF NOT EXISTS capability_revision BIGINT';
        SELECT count(*) INTO source_rows FROM gateway.assistant_runs;
        INSERT INTO assistant.assistant_runs (
            run_id, tenant_id, user_id, session_id, status, engine,
            execution_profile, memory_mode, os_agent_enabled, request_preview,
            usage, error, started_at, finished_at, created_at, updated_at,
            harness_thread_id, harness_turn_id, runtime_snapshot_id,
            kernel_revision, capability_revision
        )
        SELECT run_id, tenant_id, user_id, session_id, status, engine,
               execution_profile, memory_mode, os_agent_enabled, request_preview,
               usage, error, started_at, finished_at, created_at, updated_at,
               harness_thread_id, harness_turn_id, runtime_snapshot_id,
               kernel_revision, capability_revision
        FROM gateway.assistant_runs;
        GET DIAGNOSTICS inserted_rows = ROW_COUNT;
        IF inserted_rows <> source_rows THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: assistant_runs copied %/%',
                inserted_rows, source_rows;
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = 'assistant.assistant_runs'::regclass
              AND conname = 'assistant_runs_harness_shape_check'
        ) THEN
            ALTER TABLE assistant.assistant_runs
                ADD CONSTRAINT assistant_runs_harness_shape_check CHECK (
                    (engine <> 'agent_runtime'
                     AND harness_thread_id IS NULL
                     AND harness_turn_id IS NULL
                     AND runtime_snapshot_id IS NULL
                     AND kernel_revision IS NULL
                     AND capability_revision IS NULL)
                    OR
                    (engine = 'agent_runtime'
                     AND harness_thread_id IS NOT NULL
                     AND harness_turn_id IS NOT NULL
                     AND runtime_snapshot_id IS NOT NULL
                     AND kernel_revision IS NOT NULL
                     AND capability_revision >= 1)
                );
        END IF;
        IF to_regclass('gateway.assistant_runtime_threads') IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM pg_constraint
               WHERE conrelid = 'assistant.assistant_runs'::regclass
                 AND conname = 'assistant_runs_harness_thread_fk'
           ) THEN
            ALTER TABLE assistant.assistant_runs
                ADD CONSTRAINT assistant_runs_harness_thread_fk
                FOREIGN KEY (harness_thread_id, tenant_id, user_id, session_id)
                REFERENCES gateway.assistant_runtime_threads(
                    runtime_thread_id, tenant_id, user_id, session_id
                ) DEFERRABLE INITIALLY DEFERRED;
        END IF;
        IF to_regclass('gateway.assistant_runtime_snapshots') IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM pg_constraint
               WHERE conrelid = 'assistant.assistant_runs'::regclass
                 AND conname = 'assistant_runs_runtime_snapshot_fk'
           ) THEN
            ALTER TABLE assistant.assistant_runs
                ADD CONSTRAINT assistant_runs_runtime_snapshot_fk
                FOREIGN KEY (runtime_snapshot_id, run_id, tenant_id, user_id, session_id)
                REFERENCES gateway.assistant_runtime_snapshots(
                    snapshot_id, run_id, tenant_id, user_id, session_id
                ) DEFERRABLE INITIALLY DEFERRED;
        END IF;
        CREATE INDEX IF NOT EXISTS idx_assistant_runs_harness_thread
            ON assistant.assistant_runs(tenant_id, harness_thread_id, created_at DESC)
            WHERE harness_thread_id IS NOT NULL;
    END IF;

    IF to_regclass('gateway.assistant_tool_approvals') IS NOT NULL
       AND to_regclass('assistant.assistant_tool_approvals') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM gateway.assistant_tool_approvals AS source
            JOIN assistant.assistant_tool_approvals AS target USING (approval_id)
        ) THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: approvals overlap';
        END IF;
        ALTER TABLE assistant.assistant_tool_approvals
            ADD COLUMN IF NOT EXISTS tool_call_id VARCHAR(160);
        ALTER TABLE assistant.assistant_tool_approvals
            ALTER COLUMN tool_name TYPE VARCHAR(160);
        SELECT count(*) INTO source_rows FROM gateway.assistant_tool_approvals;
        INSERT INTO assistant.assistant_tool_approvals (
            approval_id, tenant_id, user_id, session_id, run_id, tool_name,
            arguments, status, reason, approved_by, approved_at, expires_at,
            created_at, updated_at, tool_call_id
        )
        SELECT approval_id, tenant_id, user_id, session_id, run_id, tool_name,
               arguments, status, reason, approved_by, approved_at, expires_at,
               created_at, updated_at, tool_call_id
        FROM gateway.assistant_tool_approvals;
        GET DIAGNOSTICS inserted_rows = ROW_COUNT;
        IF inserted_rows <> source_rows THEN
            RAISE EXCEPTION 'legacy duplicate reconciliation: approvals copied %/%',
                inserted_rows, source_rows;
        END IF;
        CREATE INDEX IF NOT EXISTS idx_assistant_tool_approvals_run_call
            ON assistant.assistant_tool_approvals(run_id, tool_call_id)
            WHERE tool_call_id IS NOT NULL;
    END IF;

    -- Repoint the later Runtime tables before their former parent copies are archived.
    IF to_regclass('gateway.assistant_capability_executions') IS NOT NULL THEN
        ALTER TABLE gateway.assistant_capability_executions
            DROP CONSTRAINT IF EXISTS assistant_capability_executions_run_scope_fk,
            DROP CONSTRAINT IF EXISTS assistant_capability_executions_approval_fk;
        ALTER TABLE gateway.assistant_capability_executions
            ADD CONSTRAINT assistant_capability_executions_run_scope_fk
            FOREIGN KEY (run_id, tenant_id, user_id, session_id)
            REFERENCES assistant.assistant_runs(run_id, tenant_id, user_id, session_id)
            ON DELETE RESTRICT,
            ADD CONSTRAINT assistant_capability_executions_approval_fk
            FOREIGN KEY (approval_id)
            REFERENCES assistant.assistant_tool_approvals(approval_id)
            ON DELETE RESTRICT;
    END IF;
    IF to_regclass('gateway.assistant_runtime_model_leases') IS NOT NULL THEN
        ALTER TABLE gateway.assistant_runtime_model_leases
            DROP CONSTRAINT IF EXISTS assistant_runtime_model_leases_run_fk;
        ALTER TABLE gateway.assistant_runtime_model_leases
            ADD CONSTRAINT assistant_runtime_model_leases_run_fk
            FOREIGN KEY (run_id) REFERENCES assistant.assistant_runs(run_id)
            ON DELETE RESTRICT;
    END IF;

    -- The frozen baseline intentionally keeps the retention policy in Gateway
    -- while its dataset parent is owned by Knowledge. Rebind before the empty
    -- legacy gateway.datasets copy is archived so the FK cannot follow it.
    IF to_regclass('gateway.version_retention_policies') IS NOT NULL
       AND to_regclass('knowledge.datasets') IS NOT NULL THEN
        ALTER TABLE gateway.version_retention_policies
            DROP CONSTRAINT IF EXISTS version_retention_policies_dataset_id_fkey;
        ALTER TABLE gateway.version_retention_policies
            ADD CONSTRAINT version_retention_policies_dataset_id_fkey
            FOREIGN KEY (dataset_id) REFERENCES knowledge.datasets(dataset_id)
            ON DELETE CASCADE;
    END IF;
END
$reconcile_data$;

-- Confluence objects were historically moved to Assistant before Knowledge
-- became their final owner. Both duplicate sources must be empty; the frozen
-- structural fingerprint proves the selected physical table is exact.
DO $promote_confluence$
DECLARE
    relation_name TEXT;
    row_count BIGINT;
BEGIN
    FOR relation_name IN SELECT unnest(ARRAY[
        'confluence_connections', 'confluence_image_sync', 'confluence_pages',
        'confluence_space_bindings', 'confluence_sync_tasks', 'confluence_webhooks'
    ]) LOOP
        IF to_regclass(format('knowledge.%I', relation_name)) IS NULL
           AND to_regclass(format('assistant.%I', relation_name)) IS NOT NULL
           AND to_regclass(format('gateway.%I', relation_name)) IS NOT NULL THEN
            EXECUTE format('SELECT count(*) FROM assistant.%I', relation_name)
                INTO row_count;
            IF row_count <> 0 THEN
                RAISE EXCEPTION 'legacy duplicate reconciliation: assistant.% is non-empty',
                    relation_name;
            END IF;
            EXECUTE format('SELECT count(*) FROM gateway.%I', relation_name)
                INTO row_count;
            IF row_count <> 0 THEN
                RAISE EXCEPTION 'legacy duplicate reconciliation: gateway.% is non-empty',
                    relation_name;
            END IF;
            EXECUTE format('ALTER TABLE assistant.%I SET SCHEMA knowledge', relation_name);
        END IF;
    END LOOP;
    IF to_regclass('knowledge.confluence_image_sync_id_seq') IS NULL
       AND to_regclass('assistant.confluence_image_sync_id_seq') IS NOT NULL THEN
        ALTER SEQUENCE assistant.confluence_image_sync_id_seq SET SCHEMA knowledge;
    END IF;
END
$promote_confluence$;

-- Move superseded physical copies outside every application search_path. The
-- archive is retained for restore/forensics and receives no application ACL.
DO $archive_duplicates$
DECLARE
    desired RECORD;
    extra RECORD;
    duplicate_exists BOOLEAN;
    row_count BIGINT;
    archived_name TEXT;
BEGIN
    SELECT EXISTS (
        SELECT 1
        FROM pg_class AS class
        JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND class.relkind IN ('r', 'p', 'S')
        GROUP BY class.relname
        HAVING count(DISTINCT namespace.nspname) > 1
    ) INTO duplicate_exists;
    IF NOT duplicate_exists THEN
        RETURN;
    END IF;

    EXECUTE 'CREATE SCHEMA IF NOT EXISTS platform_legacy';
    EXECUTE 'ALTER SCHEMA platform_legacy OWNER TO ai_gateway_owner';
    REVOKE ALL ON SCHEMA platform_legacy FROM PUBLIC;

    FOR desired IN SELECT * FROM (VALUES
        ('artifact_share_submitters', 'assistant'), ('artifact_shares', 'assistant'),
        ('artifacts', 'assistant'), ('assistant_audit_events', 'assistant'),
        ('assistant_command_queue', 'assistant'), ('assistant_context_breakdown', 'assistant'),
        ('assistant_memory_chunks', 'assistant'), ('assistant_memory_reflections', 'assistant'),
        ('assistant_memory_sources', 'assistant'), ('assistant_runs', 'assistant'),
        ('assistant_scheduler_jobs', 'assistant'), ('assistant_skill_runs', 'assistant'),
        ('assistant_skill_versions', 'assistant'), ('assistant_skills', 'assistant'),
        ('assistant_tool_approvals', 'assistant'), ('conversation_shares', 'assistant'),
        ('session_memory', 'assistant'), ('sessions', 'assistant'), ('user_memory', 'assistant'),
        ('child_chunks', 'knowledge'), ('confluence_connections', 'knowledge'),
        ('confluence_image_sync', 'knowledge'), ('confluence_image_sync_id_seq', 'knowledge'),
        ('confluence_pages', 'knowledge'), ('confluence_space_bindings', 'knowledge'),
        ('confluence_sync_tasks', 'knowledge'), ('confluence_webhooks', 'knowledge'),
        ('dataset_keyword_tables', 'knowledge'), ('dataset_permissions', 'knowledge'),
        ('dataset_permissions_id_seq', 'knowledge'), ('dataset_process_rules', 'knowledge'),
        ('dataset_queries', 'knowledge'), ('datasets', 'knowledge'),
        ('document_versions', 'knowledge'), ('documents', 'knowledge'),
        ('segment_images', 'knowledge'), ('segment_images_id_seq', 'knowledge'),
        ('segments', 'knowledge'), ('version_retention_policies', 'gateway'),
        ('schema_migrations', 'public')
    ) AS mapping(relation_name, target_schema) LOOP
        IF to_regclass(format('%I.%I', desired.target_schema, desired.relation_name)) IS NULL THEN
            CONTINUE;
        END IF;
        FOR extra IN
            SELECT namespace.nspname AS source_schema, class.relkind
            FROM pg_class AS class
            JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
            WHERE class.relname = desired.relation_name
              AND class.relkind IN ('r', 'p', 'S')
              AND namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
              AND namespace.nspname <> desired.target_schema
            ORDER BY namespace.nspname
        LOOP
            IF extra.relkind IN ('r', 'p')
               AND desired.relation_name NOT IN (
                   'assistant_runs', 'assistant_tool_approvals',
                   'session_memory', 'sessions', 'user_memory'
               ) THEN
                EXECUTE format(
                    'SELECT count(*) FROM %I.%I',
                    extra.source_schema, desired.relation_name
                ) INTO row_count;
                IF row_count <> 0 THEN
                    RAISE EXCEPTION
                        'legacy duplicate reconciliation: unexpected data in %.% (% rows)',
                        extra.source_schema, desired.relation_name, row_count;
                END IF;
            END IF;
            archived_name := extra.source_schema || '__' || desired.relation_name;
            IF to_regclass(format('platform_legacy.%I', archived_name)) IS NOT NULL THEN
                RAISE EXCEPTION 'legacy duplicate archive collision: %', archived_name;
            END IF;
            IF extra.relkind = 'S' THEN
                EXECUTE format(
                    'ALTER SEQUENCE %I.%I SET SCHEMA platform_legacy',
                    extra.source_schema, desired.relation_name
                );
                EXECUTE format(
                    'ALTER SEQUENCE platform_legacy.%I RENAME TO %I',
                    desired.relation_name, archived_name
                );
                EXECUTE format(
                    'ALTER SEQUENCE platform_legacy.%I OWNER TO ai_gateway_owner',
                    archived_name
                );
            ELSE
                EXECUTE format(
                    'ALTER TABLE %I.%I SET SCHEMA platform_legacy',
                    extra.source_schema, desired.relation_name
                );
                EXECUTE format(
                    'ALTER TABLE platform_legacy.%I RENAME TO %I',
                    desired.relation_name, archived_name
                );
                EXECUTE format(
                    'ALTER TABLE platform_legacy.%I OWNER TO ai_gateway_owner',
                    archived_name
                );
            END IF;
        END LOOP;
    END LOOP;

    REVOKE ALL ON ALL TABLES IN SCHEMA platform_legacy FROM PUBLIC;
    REVOKE ALL ON ALL SEQUENCES IN SCHEMA platform_legacy FROM PUBLIC;
END
$archive_duplicates$;

-- Phase-6 also left a small, fully enumerated set of duplicate routines. The
-- bodies and execution attributes must be identical before one physical
-- routine is selected. Every trigger is rebound to the desired routine before
-- the redundant OID is archived, so no active table retains an archive edge.
DO $archive_duplicate_functions$
DECLARE
    desired RECORD;
    extra RECORD;
    trigger_row RECORD;
    matches BIGINT;
    variants BIGINT;
    trigger_definition TEXT;
    archived_name TEXT;
BEGIN
    FOR desired IN SELECT * FROM (VALUES
        ('cleanup_old_usage_records', 'integer', 'gateway', 'gateway'),
        ('segments_text_search_update', '', 'public', 'knowledge'),
        ('update_assistant_gateway_timestamp', '', 'public', 'assistant'),
        ('update_assistant_memory_chunks_text_search', '', 'public', 'assistant'),
        ('update_memory_timestamp', '', 'public', 'assistant'),
        ('update_segment_image_counts', '', 'public', 'knowledge'),
        ('update_updated_at_column', '', 'gateway', 'gateway')
    ) AS mapping(name, identity_arguments, canonical_schema, target_schema)
    LOOP
        SELECT count(*), count(DISTINCT md5(
            procedure.prosrc || ':' || procedure.prolang::text || ':' ||
            procedure.prorettype::text || ':' || procedure.provolatile::text || ':' ||
            procedure.prosecdef::text || ':' || procedure.proisstrict::text || ':' ||
            COALESCE(array_to_string(procedure.proconfig, ','), '')
        ))
        INTO matches, variants
        FROM pg_proc AS procedure
        JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND procedure.proname = desired.name
          AND oidvectortypes(procedure.proargtypes) = desired.identity_arguments
          AND procedure.prokind = 'f';
        IF matches <= 1 THEN
            CONTINUE;
        END IF;
        IF variants <> 1 THEN
            RAISE EXCEPTION
                'legacy duplicate reconciliation: function %(%) has % semantic variants',
                desired.name, desired.identity_arguments, variants;
        END IF;
        IF to_regprocedure(format(
            '%I.%I(%s)', desired.canonical_schema, desired.name,
            desired.identity_arguments
        )) IS NULL THEN
            RAISE EXCEPTION
                'legacy duplicate reconciliation: canonical function %.%(%) is absent',
                desired.canonical_schema, desired.name, desired.identity_arguments;
        END IF;
        IF desired.canonical_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER FUNCTION %I.%I(%s) SET SCHEMA %I',
                desired.canonical_schema, desired.name, desired.identity_arguments,
                desired.target_schema
            );
        END IF;

        EXECUTE 'CREATE SCHEMA IF NOT EXISTS platform_legacy';
        EXECUTE 'ALTER SCHEMA platform_legacy OWNER TO ai_gateway_owner';
        FOR extra IN
            SELECT procedure.oid, namespace.nspname AS source_schema
            FROM pg_proc AS procedure
            JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
            WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
              AND namespace.nspname <> desired.target_schema
              AND procedure.proname = desired.name
              AND oidvectortypes(procedure.proargtypes) = desired.identity_arguments
              AND procedure.prokind = 'f'
            ORDER BY namespace.nspname
        LOOP
            FOR trigger_row IN
                SELECT trigger.oid
                FROM pg_trigger AS trigger
                WHERE trigger.tgfoid = extra.oid AND NOT trigger.tgisinternal
            LOOP
                trigger_definition := pg_get_triggerdef(trigger_row.oid, true);
                trigger_definition := replace(
                    trigger_definition, 'CREATE TRIGGER', 'CREATE OR REPLACE TRIGGER'
                );
                trigger_definition := regexp_replace(
                    trigger_definition,
                    'EXECUTE FUNCTION [^(]+\(',
                    format('EXECUTE FUNCTION %I.%I(', desired.target_schema, desired.name)
                );
                EXECUTE trigger_definition;
            END LOOP;

            archived_name := extra.source_schema || '__' || desired.name;
            IF to_regprocedure(format(
                'platform_legacy.%I(%s)', archived_name, desired.identity_arguments
            )) IS NOT NULL THEN
                RAISE EXCEPTION 'legacy duplicate function archive collision: %',
                    archived_name;
            END IF;
            EXECUTE format(
                'ALTER FUNCTION %I.%I(%s) SET SCHEMA platform_legacy',
                extra.source_schema, desired.name, desired.identity_arguments
            );
            EXECUTE format(
                'ALTER FUNCTION platform_legacy.%I(%s) RENAME TO %I',
                desired.name, desired.identity_arguments, archived_name
            );
            EXECUTE format(
                'ALTER FUNCTION platform_legacy.%I(%s) OWNER TO ai_gateway_owner',
                archived_name, desired.identity_arguments
            );
        END LOOP;
    END LOOP;
    REVOKE ALL ON SCHEMA platform_legacy FROM PUBLIC;
    REVOKE ALL ON ALL FUNCTIONS IN SCHEMA platform_legacy FROM PUBLIC;
END
$archive_duplicate_functions$;

-- No live application object may retain an ownership edge into the archive.
-- Moving a referenced relation or routine preserves its OID, so this explicit
-- check is required in addition to the frozen structural fingerprint.
DO $assert_archive_is_detached$
DECLARE
    dangling_edges BIGINT;
BEGIN
    SELECT count(*) INTO dangling_edges
    FROM pg_constraint AS constraint_row
    JOIN pg_class AS source_class ON source_class.oid = constraint_row.conrelid
    JOIN pg_namespace AS source_namespace
      ON source_namespace.oid = source_class.relnamespace
    JOIN pg_class AS target_class ON target_class.oid = constraint_row.confrelid
    JOIN pg_namespace AS target_namespace
      ON target_namespace.oid = target_class.relnamespace
    WHERE source_namespace.nspname IN ('gateway', 'assistant', 'knowledge')
      AND target_namespace.nspname = 'platform_legacy';

    SELECT dangling_edges + count(*) INTO dangling_edges
    FROM pg_trigger AS trigger_row
    JOIN pg_class AS source_class ON source_class.oid = trigger_row.tgrelid
    JOIN pg_namespace AS source_namespace
      ON source_namespace.oid = source_class.relnamespace
    JOIN pg_proc AS target_function ON target_function.oid = trigger_row.tgfoid
    JOIN pg_namespace AS target_namespace
      ON target_namespace.oid = target_function.pronamespace
    WHERE NOT trigger_row.tgisinternal
      AND source_namespace.nspname IN ('gateway', 'assistant', 'knowledge')
      AND target_namespace.nspname = 'platform_legacy';

    SELECT dangling_edges + count(*) INTO dangling_edges
    FROM pg_attrdef AS default_row
    JOIN pg_class AS source_class ON source_class.oid = default_row.adrelid
    JOIN pg_namespace AS source_namespace
      ON source_namespace.oid = source_class.relnamespace
    JOIN pg_depend AS dependency
      ON dependency.classid = 'pg_attrdef'::regclass
     AND dependency.objid = default_row.oid
     AND dependency.refclassid = 'pg_class'::regclass
    JOIN pg_class AS target_class ON target_class.oid = dependency.refobjid
    JOIN pg_namespace AS target_namespace
      ON target_namespace.oid = target_class.relnamespace
    WHERE source_namespace.nspname IN ('gateway', 'assistant', 'knowledge')
      AND target_namespace.nspname = 'platform_legacy';

    IF dangling_edges <> 0 THEN
        RAISE EXCEPTION
            'legacy duplicate reconciliation: % live archive dependencies remain',
            dangling_edges;
    END IF;
END
$assert_archive_is_detached$;
