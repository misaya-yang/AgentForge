-- =============================================================================
-- Phase 6 — move tables from public to per-service schemas.
-- =============================================================================
-- Idempotent: each ALTER TABLE wrapped in DO block that no-ops if the table
-- is already in the destination schema (or doesn't exist on this DB).
--
-- ALTER TABLE … SET SCHEMA … takes ACCESS EXCLUSIVE on the table for the
-- rename only — sub-second per table, FKs survive intact (PG stores FKs by
-- oid, not by name). Total observed wall time across ~85 tables: 2-4s.
--
-- Cross-schema FKs are fine (PG supports them). Sequences and indexes move
-- with their owning table automatically.
--
-- Strategy:
--   1. ALTER each table to its destination schema.
--   2. Tables not listed here stay in public — that's OK; they'll surface
--      in the next migration as "decide where this lives".
--
-- Tables not owned by gateway/assistant/knowledge stay untouched.
-- =============================================================================

-- (psql metacommand removed — asyncpg runs the file directly. The runner
-- already wraps each file in a transaction so any error rolls back.)

DO $migrate$
DECLARE
    rec        RECORD;
    moves      JSONB := jsonb_build_array(
        -- ====================  GATEWAY SCHEMA  ====================
        -- Auth + identity
        jsonb_build_object('table', 'users',                              'to', 'gateway'),
        jsonb_build_object('table', 'tenants',                            'to', 'gateway'),
        jsonb_build_object('table', 'api_keys',                           'to', 'gateway'),
        jsonb_build_object('table', 'auth_config',                        'to', 'gateway'),
        jsonb_build_object('table', 'rbac_roles',                         'to', 'gateway'),
        jsonb_build_object('table', 'permissions',                        'to', 'gateway'),
        jsonb_build_object('table', 'role_permissions',                   'to', 'gateway'),
        jsonb_build_object('table', 'user_roles',                         'to', 'gateway'),
        jsonb_build_object('table', 'user_permissions',                   'to', 'gateway'),
        jsonb_build_object('table', 'email_domain_config',                'to', 'gateway'),
        jsonb_build_object('table', 'login_audit',                        'to', 'gateway'),
        jsonb_build_object('table', 'password_history',                   'to', 'gateway'),
        -- Rate limit + audit + observability
        jsonb_build_object('table', 'rate_limit_config',                  'to', 'gateway'),
        jsonb_build_object('table', 'audit_logs',                         'to', 'gateway'),
        jsonb_build_object('table', 'request_traces',                     'to', 'gateway'),
        jsonb_build_object('table', 'security_event_daily_aggregates',    'to', 'gateway'),
        -- Service registry + LLM
        jsonb_build_object('table', 'services',                           'to', 'gateway'),
        jsonb_build_object('table', 'service_health_records',             'to', 'gateway'),
        jsonb_build_object('table', 'llm_providers',                      'to', 'gateway'),
        jsonb_build_object('table', 'llm_models',                         'to', 'gateway'),
        jsonb_build_object('table', 'model_pricing',                      'to', 'gateway'),
        jsonb_build_object('table', 'semantic_cache',                     'to', 'gateway'),
        jsonb_build_object('table', 'proxy_routes',                       'to', 'gateway'),
        jsonb_build_object('table', 'langgraph_threads',                  'to', 'gateway'),
        -- Billing + quota
        jsonb_build_object('table', 'usage_records',                      'to', 'gateway'),
        jsonb_build_object('table', 'usage_daily_aggregates',             'to', 'gateway'),
        jsonb_build_object('table', 'usage_hourly_aggregates',            'to', 'gateway'),
        jsonb_build_object('table', 'usage_statistics',                   'to', 'gateway'),
        jsonb_build_object('table', 'user_quotas',                        'to', 'gateway'),
        jsonb_build_object('table', 'quota_alerts',                       'to', 'gateway'),
        jsonb_build_object('table', 'billing_events',                     'to', 'gateway'),
        -- Tasks + tools + tenant policies
        jsonb_build_object('table', 'tasks',                              'to', 'gateway'),
        jsonb_build_object('table', 'tenant_mcp_configs',                 'to', 'gateway'),
        jsonb_build_object('table', 'tenant_tool_policies',               'to', 'gateway'),
        jsonb_build_object('table', 'tool_audit_log',                     'to', 'gateway'),

        -- ====================  ASSISTANT SCHEMA  ====================
        -- Sessions + memory
        jsonb_build_object('table', 'sessions',                           'to', 'assistant'),
        jsonb_build_object('table', 'session_memory',                     'to', 'assistant'),
        jsonb_build_object('table', 'user_memory',                        'to', 'assistant'),
        -- Agent loop state
        jsonb_build_object('table', 'assistant_runs',                     'to', 'assistant'),
        jsonb_build_object('table', 'assistant_command_queue',            'to', 'assistant'),
        jsonb_build_object('table', 'assistant_tool_approvals',           'to', 'assistant'),
        jsonb_build_object('table', 'assistant_audit_events',             'to', 'assistant'),
        jsonb_build_object('table', 'assistant_context_breakdown',        'to', 'assistant'),
        jsonb_build_object('table', 'assistant_memory_sources',           'to', 'assistant'),
        jsonb_build_object('table', 'assistant_memory_chunks',            'to', 'assistant'),
        jsonb_build_object('table', 'assistant_memory_reflections',       'to', 'assistant'),
        jsonb_build_object('table', 'assistant_skills',                   'to', 'assistant'),
        jsonb_build_object('table', 'assistant_skill_versions',           'to', 'assistant'),
        jsonb_build_object('table', 'assistant_skill_runs',               'to', 'assistant'),
        jsonb_build_object('table', 'assistant_scheduler_jobs',           'to', 'assistant'),
        -- Artifacts + connectors
        jsonb_build_object('table', 'artifacts',                          'to', 'assistant'),
        jsonb_build_object('table', 'user_connectors',                    'to', 'assistant'),
        jsonb_build_object('table', 'connector_configs',                  'to', 'assistant'),
        -- Quizzes + exams + sharing
        jsonb_build_object('table', 'quizzes',                            'to', 'assistant'),
        jsonb_build_object('table', 'quiz_questions',                     'to', 'assistant'),
        jsonb_build_object('table', 'quiz_attempts',                      'to', 'assistant'),
        jsonb_build_object('table', 'quiz_shares',                        'to', 'assistant'),
        jsonb_build_object('table', 'exams',                              'to', 'assistant'),
        jsonb_build_object('table', 'exam_analysis_reports',              'to', 'assistant'),
        jsonb_build_object('table', 'conversation_shares',                'to', 'assistant'),
        jsonb_build_object('table', 'conversation_share_quiz_attempts',   'to', 'assistant'),
        -- Confluence integration
        jsonb_build_object('table', 'confluence_connections',             'to', 'assistant'),
        jsonb_build_object('table', 'confluence_space_bindings',          'to', 'assistant'),
        jsonb_build_object('table', 'confluence_pages',                   'to', 'assistant'),
        jsonb_build_object('table', 'confluence_sync_tasks',              'to', 'assistant'),
        jsonb_build_object('table', 'confluence_webhooks',                'to', 'assistant'),
        jsonb_build_object('table', 'confluence_image_sync',              'to', 'assistant'),

        -- ====================  KNOWLEDGE SCHEMA  ====================
        jsonb_build_object('table', 'datasets',                           'to', 'knowledge'),
        jsonb_build_object('table', 'documents',                          'to', 'knowledge'),
        jsonb_build_object('table', 'segments',                           'to', 'knowledge'),
        jsonb_build_object('table', 'segment_images',                     'to', 'knowledge'),
        jsonb_build_object('table', 'child_chunks',                       'to', 'knowledge'),
        jsonb_build_object('table', 'dataset_permissions',                'to', 'knowledge'),
        jsonb_build_object('table', 'dataset_keyword_tables',             'to', 'knowledge'),
        jsonb_build_object('table', 'dataset_process_rules',              'to', 'knowledge'),
        jsonb_build_object('table', 'dataset_queries',                    'to', 'knowledge'),
        jsonb_build_object('table', 'document_versions',                  'to', 'knowledge'),
        jsonb_build_object('table', 'document_summaries',                 'to', 'knowledge'),
        jsonb_build_object('table', 'version_retention_policies',         'to', 'knowledge'),
        jsonb_build_object('table', 'source_snapshots',                   'to', 'knowledge'),
        jsonb_build_object('table', 'source_sync_runs',                   'to', 'knowledge')
    );
    cur_schema TEXT;
    moved_count INT := 0;
    skipped_count INT := 0;
BEGIN
    FOR rec IN SELECT (m->>'table') AS table_name, (m->>'to') AS dest_schema
               FROM jsonb_array_elements(moves) AS m
    LOOP
        -- Find current schema (NULL if table doesn't exist on this DB).
        SELECT n.nspname INTO cur_schema
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = rec.table_name
          AND c.relkind = 'r'
          AND n.nspname IN ('public', rec.dest_schema)
        ORDER BY (n.nspname = rec.dest_schema) DESC  -- prefer dest if both visible
        LIMIT 1;

        IF cur_schema IS NULL THEN
            RAISE NOTICE 'skip % — table not present on this DB', rec.table_name;
            skipped_count := skipped_count + 1;
            CONTINUE;
        END IF;

        IF cur_schema = rec.dest_schema THEN
            RAISE NOTICE 'skip % — already in %', rec.table_name, rec.dest_schema;
            skipped_count := skipped_count + 1;
            CONTINUE;
        END IF;

        EXECUTE format('ALTER TABLE %I.%I SET SCHEMA %I',
                       cur_schema, rec.table_name, rec.dest_schema);
        moved_count := moved_count + 1;
        RAISE NOTICE 'moved %.% -> %', cur_schema, rec.table_name, rec.dest_schema;
    END LOOP;

    RAISE NOTICE 'phase6 schema-split complete: % moved, % skipped',
                 moved_count, skipped_count;
END
$migrate$;

INSERT INTO public.schema_migrations_meta(name, notes)
VALUES ('phase6_tables_moved', 'tables relocated from public to gateway/assistant/knowledge')
ON CONFLICT (name) DO NOTHING;
