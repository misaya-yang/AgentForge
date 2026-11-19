-- ============================================================================
-- 2026_08_post_kb_v1 cutover: schema convergence change (PRD ARC-03 §3A)
-- ============================================================================
-- Runs against EXISTING databases only, after their legacy ledger is proven
-- complete up to the freeze point, and before fingerprint comparison /
-- adoption.  Fresh installs never execute this file: init.sql already
-- carries the converged shape plus the same ownership tail.
--
-- The change is idempotent and fail-closed:
--   * same-named objects in two platform schemas abort the cutover (no
--     automatic merge, no automatic drop);
--   * every persistent object in the platform schemas is re-owned to the
--     NOLOGIN owner role;
--   * SECURITY DEFINER functions get a pinned safe search_path;
--   * PUBLIC loses CREATE on schemas and every residual grant.
-- The runner owns the transaction; this file carries no BEGIN/COMMIT.
-- ============================================================================

-- 0. Fail closed on same-named objects across platform schemas ------------
DO $$
DECLARE
    duplicates TEXT;
BEGIN
    SELECT string_agg(DISTINCT c.relname, ', ' ORDER BY c.relname)
    INTO duplicates
    FROM (
        SELECT c.relname, count(DISTINCT n.nspname) AS schema_count
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND c.relkind IN ('r', 'p', 'm', 'S', 'v', 'f')
          AND NOT EXISTS (
              SELECT 1
              FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_class'::regclass
                AND dependency.objid = c.oid
                AND dependency.deptype = 'e'
          )
        GROUP BY c.relname
        HAVING count(DISTINCT n.nspname) > 1
    ) AS ambiguous
    JOIN pg_class AS c ON c.relname = ambiguous.relname;

    IF duplicates IS NOT NULL THEN
        RAISE EXCEPTION
            'schema convergence refused: same-named objects exist in multiple platform schemas: %',
            duplicates;
    END IF;
END
$$;

-- 1. Platform schemas exist -------------------------------------------------
CREATE SCHEMA IF NOT EXISTS gateway;
CREATE SCHEMA IF NOT EXISTS assistant;
CREATE SCHEMA IF NOT EXISTS knowledge;
-- Close the name-hijack window before inspecting or hardening any routine.
REVOKE CREATE ON SCHEMA public, gateway, assistant, knowledge FROM PUBLIC;

-- BEGIN ARC03 GENERATED OBJECT RELOCATION
-- Generated from ownership-policy.json; DO NOT EDIT THIS BLOCK.
-- Missing or duplicate source objects abort before any relocation is accepted.
DO $$
DECLARE
    desired RECORD;
    source_schema TEXT;
    source_relkind TEXT;
    matches BIGINT;
BEGIN
    FOR desired IN
        SELECT * FROM (
            VALUES
            ('sequence', 'api_keys_id_seq', 'gateway'),
            ('sequence', 'audit_logs_id_seq', 'gateway'),
            ('sequence', 'auth_config_id_seq', 'gateway'),
            ('sequence', 'billing_events_id_seq', 'gateway'),
            ('sequence', 'confluence_image_sync_id_seq', 'knowledge'),
            ('sequence', 'dataset_permissions_id_seq', 'knowledge'),
            ('sequence', 'email_domain_config_id_seq', 'gateway'),
            ('sequence', 'kb_document_progress_events_event_sequence_seq', 'knowledge'),
            ('sequence', 'langgraph_threads_id_seq', 'gateway'),
            ('sequence', 'local_node_events_device_sequence_seq', 'gateway'),
            ('sequence', 'login_audit_id_seq', 'gateway'),
            ('sequence', 'password_history_id_seq', 'gateway'),
            ('sequence', 'permissions_id_seq', 'gateway'),
            ('sequence', 'proxy_routes_id_seq', 'gateway'),
            ('sequence', 'rate_limit_config_id_seq', 'gateway'),
            ('sequence', 'rbac_roles_id_seq', 'gateway'),
            ('sequence', 'role_permissions_id_seq', 'gateway'),
            ('sequence', 'segment_images_id_seq', 'knowledge'),
            ('sequence', 'semantic_cache_id_seq', 'gateway'),
            ('sequence', 'service_health_records_id_seq', 'gateway'),
            ('sequence', 'tenants_id_seq', 'gateway'),
            ('sequence', 'tool_audit_log_id_seq', 'gateway'),
            ('sequence', 'usage_statistics_id_seq', 'gateway'),
            ('sequence', 'user_permissions_id_seq', 'gateway'),
            ('sequence', 'user_roles_id_seq', 'gateway'),
            ('sequence', 'users_id_seq', 'gateway'),
            ('table', 'agent_api_tokens', 'gateway'),
            ('table', 'agent_data_deletion_requests', 'gateway'),
            ('table', 'agent_draft_knowledge_bindings', 'gateway'),
            ('table', 'agent_draft_skill_bindings', 'gateway'),
            ('table', 'agent_drafts', 'gateway'),
            ('table', 'agent_governance_policies', 'gateway'),
            ('table', 'agent_members', 'gateway'),
            ('table', 'agent_publications', 'gateway'),
            ('table', 'agent_publish_events', 'gateway'),
            ('table', 'agent_release_evaluation_events', 'gateway'),
            ('table', 'agent_release_evaluations', 'gateway'),
            ('table', 'agent_release_requests', 'gateway'),
            ('table', 'agent_runtime_attachments', 'assistant'),
            ('table', 'agent_runtime_feedback', 'assistant'),
            ('table', 'agent_runtime_idempotency', 'assistant'),
            ('table', 'agent_trace_events', 'gateway'),
            ('table', 'agent_trace_outbox', 'gateway'),
            ('table', 'agent_trace_scores', 'gateway'),
            ('table', 'agent_trace_spans', 'gateway'),
            ('table', 'agent_traces', 'gateway'),
            ('table', 'agent_version_capabilities', 'gateway'),
            ('table', 'agent_version_knowledge_bindings', 'gateway'),
            ('table', 'agent_version_revocations', 'gateway'),
            ('table', 'agent_version_skill_bindings', 'gateway'),
            ('table', 'agent_versions', 'gateway'),
            ('table', 'agents', 'gateway'),
            ('table', 'api_keys', 'gateway'),
            ('table', 'artifact_share_attempt_tokens', 'assistant'),
            ('table', 'artifact_share_submitters', 'assistant'),
            ('table', 'artifact_shares', 'assistant'),
            ('table', 'artifacts', 'assistant'),
            ('table', 'assistant_audit_events', 'assistant'),
            ('table', 'assistant_capability_events', 'assistant'),
            ('table', 'assistant_capability_executions', 'assistant'),
            ('table', 'assistant_command_queue', 'assistant'),
            ('table', 'assistant_context_breakdown', 'assistant'),
            ('table', 'assistant_memory_chunks', 'assistant'),
            ('table', 'assistant_memory_reflections', 'assistant'),
            ('table', 'assistant_memory_sources', 'assistant'),
            ('table', 'assistant_run_checkpoints', 'assistant'),
            ('table', 'assistant_runs', 'assistant'),
            ('table', 'assistant_runtime_items', 'assistant'),
            ('table', 'assistant_runtime_model_calls', 'assistant'),
            ('table', 'assistant_runtime_model_leases', 'assistant'),
            ('table', 'assistant_runtime_snapshot_revocations', 'assistant'),
            ('table', 'assistant_runtime_snapshots', 'assistant'),
            ('table', 'assistant_runtime_thread_members', 'assistant'),
            ('table', 'assistant_runtime_thread_projections', 'assistant'),
            ('table', 'assistant_runtime_threads', 'assistant'),
            ('table', 'assistant_scheduler_jobs', 'assistant'),
            ('table', 'assistant_session_runtime_assignments', 'assistant'),
            ('table', 'assistant_skill_runs', 'assistant'),
            ('table', 'assistant_skill_version_revocations', 'assistant'),
            ('table', 'assistant_skill_versions', 'assistant'),
            ('table', 'assistant_skills', 'assistant'),
            ('table', 'assistant_tool_approvals', 'assistant'),
            ('table', 'audit_logs', 'gateway'),
            ('table', 'auth_config', 'gateway'),
            ('table', 'billing_events', 'gateway'),
            ('table', 'child_chunks', 'knowledge'),
            ('table', 'confluence_connections', 'knowledge'),
            ('table', 'confluence_image_sync', 'knowledge'),
            ('table', 'confluence_pages', 'knowledge'),
            ('table', 'confluence_space_bindings', 'knowledge'),
            ('table', 'confluence_sync_tasks', 'knowledge'),
            ('table', 'confluence_webhooks', 'knowledge'),
            ('table', 'connector_configs', 'gateway'),
            ('table', 'connector_credential_principals', 'gateway'),
            ('table', 'conversation_share_quiz_attempts', 'assistant'),
            ('table', 'conversation_shares', 'assistant'),
            ('table', 'dataset_collection_bindings', 'knowledge'),
            ('table', 'dataset_keyword_tables', 'knowledge'),
            ('table', 'dataset_permissions', 'knowledge'),
            ('table', 'dataset_process_rules', 'knowledge'),
            ('table', 'dataset_queries', 'knowledge'),
            ('table', 'dataset_query_feedback', 'knowledge'),
            ('table', 'datasets', 'knowledge'),
            ('table', 'document_pipeline_executions', 'knowledge'),
            ('table', 'document_summaries', 'knowledge'),
            ('table', 'document_versions', 'knowledge'),
            ('table', 'documents', 'knowledge'),
            ('table', 'email_domain_config', 'gateway'),
            ('table', 'embedding_migration_action_jobs', 'knowledge'),
            ('table', 'embedding_migration_progress', 'knowledge'),
            ('table', 'embedding_migrations', 'knowledge'),
            ('table', 'embedding_vector_cache', 'knowledge'),
            ('table', 'eval_baseline_promotions', 'gateway'),
            ('table', 'eval_datasets', 'gateway'),
            ('table', 'eval_evaluators', 'gateway'),
            ('table', 'eval_examples', 'gateway'),
            ('table', 'eval_experiment_run_cases', 'gateway'),
            ('table', 'eval_experiment_runs', 'gateway'),
            ('table', 'eval_experiments', 'gateway'),
            ('table', 'exam_analysis_reports', 'assistant'),
            ('table', 'exams', 'assistant'),
            ('table', 'image_blobs', 'assistant'),
            ('table', 'image_idempotency', 'assistant'),
            ('table', 'image_sessions', 'assistant'),
            ('table', 'image_tasks', 'assistant'),
            ('table', 'image_turns', 'assistant'),
            ('table', 'kb_bm25_v2_lifecycle', 'knowledge'),
            ('table', 'kb_document_batch_items', 'knowledge'),
            ('table', 'kb_document_batch_operations', 'knowledge'),
            ('table', 'kb_document_progress_events', 'knowledge'),
            ('table', 'kb_eval_golden', 'knowledge'),
            ('table', 'kb_eval_golden_release', 'knowledge'),
            ('table', 'kb_parsing_ir', 'knowledge'),
            ('table', 'kb_parsing_page_cache', 'knowledge'),
            ('table', 'kb_segment_attachment_bindings', 'knowledge'),
            ('table', 'langgraph_threads', 'gateway'),
            ('table', 'llm_models', 'gateway'),
            ('table', 'llm_providers', 'gateway'),
            ('table', 'local_node_channels', 'gateway'),
            ('table', 'local_node_devices', 'gateway'),
            ('table', 'local_node_events', 'gateway'),
            ('table', 'local_node_executions', 'gateway'),
            ('table', 'local_node_grants', 'gateway'),
            ('table', 'local_node_pairing_challenges', 'gateway'),
            ('table', 'local_node_receipts', 'gateway'),
            ('table', 'login_audit', 'gateway'),
            ('table', 'mcp_channel_grants', 'gateway'),
            ('table', 'mcp_connections', 'gateway'),
            ('table', 'mcp_schema_diffs', 'gateway'),
            ('table', 'mcp_servers', 'gateway'),
            ('table', 'mcp_tool_snapshots', 'gateway'),
            ('table', 'mcp_tools', 'gateway'),
            ('table', 'model_pricing', 'gateway'),
            ('table', 'password_history', 'gateway'),
            ('table', 'permissions', 'gateway'),
            ('table', 'proxy_routes', 'gateway'),
            ('table', 'quiz_attempts', 'assistant'),
            ('table', 'quiz_questions', 'assistant'),
            ('table', 'quiz_shares', 'assistant'),
            ('table', 'quizzes', 'assistant'),
            ('table', 'quota_alerts', 'gateway'),
            ('table', 'rate_limit_config', 'gateway'),
            ('table', 'rbac_roles', 'gateway'),
            ('table', 'request_traces', 'gateway'),
            ('table', 'role_permissions', 'gateway'),
            ('table', 'security_event_daily_aggregates', 'gateway'),
            ('table', 'segment_images', 'knowledge'),
            ('table', 'segments', 'knowledge'),
            ('table', 'semantic_cache', 'gateway'),
            ('table', 'service_health_records', 'gateway'),
            ('table', 'services', 'gateway'),
            ('table', 'session_memory', 'assistant'),
            ('table', 'sessions', 'assistant'),
            ('table', 'tasks', 'gateway'),
            ('table', 'tenant_mcp_configs', 'gateway'),
            ('table', 'tenant_tool_policies', 'gateway'),
            ('table', 'tenants', 'gateway'),
            ('table', 'tool_audit_log', 'gateway'),
            ('table', 'usage_daily_aggregates', 'gateway'),
            ('table', 'usage_hourly_aggregates', 'gateway'),
            ('table', 'usage_records', 'gateway'),
            ('table', 'usage_statistics', 'gateway'),
            ('table', 'user_connectors', 'gateway'),
            ('table', 'user_memory', 'assistant'),
            ('table', 'user_permissions', 'gateway'),
            ('table', 'user_quotas', 'gateway'),
            ('table', 'user_roles', 'gateway'),
            ('table', 'users', 'gateway'),
            ('table', 'version_retention_policies', 'gateway'),
            ('view', 'v_active_proxy_services', 'gateway'),
            ('view', 'v_user_billing_summary', 'gateway')
        ) AS mapping(kind, name, target_schema)
        -- PostgreSQL moves an owned serial/identity sequence together with
        -- its table and rejects moving that sequence independently first.
        -- Relocate tables/views before sequences; the later sequence row then
        -- observes the already-correct target schema and remains a full
        -- existence/uniqueness check rather than a skipped object.
        ORDER BY CASE kind
                     WHEN 'table' THEN 0
                     WHEN 'view' THEN 1
                     WHEN 'sequence' THEN 2
                     ELSE 3
                 END,
                 name
    LOOP
        SELECT count(*), min(namespace.nspname), min(class.relkind::text)
        INTO matches, source_schema, source_relkind
        FROM pg_class AS class
        JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND class.relname = desired.name
          AND CASE desired.kind
              WHEN 'table' THEN class.relkind IN ('r', 'p', 'f')
              WHEN 'view' THEN class.relkind IN ('v', 'm')
              WHEN 'sequence' THEN class.relkind = 'S'
              ELSE FALSE
          END
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_class'::regclass
                AND dependency.objid = class.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation % % matched % objects',
                desired.kind, desired.name, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER %s %I.%I SET SCHEMA %I',
                CASE
                    WHEN source_relkind = 'S' THEN 'SEQUENCE'
                    WHEN source_relkind = 'v' THEN 'VIEW'
                    WHEN source_relkind = 'm' THEN 'MATERIALIZED VIEW'
                    WHEN source_relkind = 'f' THEN 'FOREIGN TABLE'
                    ELSE 'TABLE'
                END,
                source_schema, desired.name, desired.target_schema
            );
        END IF;
    END LOOP;

    FOR desired IN
        SELECT * FROM (
            VALUES
            ('agent_audit_dimension_projection', '', 'gateway'),
            ('agent_audit_redact_jsonb', 'jsonb', 'gateway'),
            ('agent_data_deletion_terminal_guard', '', 'gateway'),
            ('agent_knowledge_bump_content_revision', '', 'gateway'),
            ('agent_runtime_reject_revocation_mutation', '', 'gateway'),
            ('agent_skill_reject_version_mutation', '', 'gateway'),
            ('agent_studio_assert_owner_invariant', '', 'gateway'),
            ('agent_studio_guard_release_evaluation_lifecycle', '', 'gateway'),
            ('agent_studio_protect_last_owner', '', 'gateway'),
            ('agent_studio_protect_member_identity', '', 'gateway'),
            ('agent_studio_protect_version_binding_mutation', '', 'gateway'),
            ('agent_studio_protect_version_mutation', '', 'gateway'),
            ('agent_studio_reject_immutable_mutation', '', 'gateway'),
            ('agent_studio_reject_release_evidence_mutation', '', 'gateway'),
            ('agent_studio_require_sealed_version', '', 'gateway'),
            ('agent_studio_touch_updated_at', '', 'gateway'),
            ('append_assistant_capability_event', 'uuid, character varying, character varying, character varying, uuid, character varying, character varying, jsonb, uuid', 'assistant'),
            ('append_assistant_runtime_item', 'uuid, uuid, character varying, character varying, character varying, uuid, character varying, character varying, character varying, character varying, character varying, character varying, jsonb, character', 'assistant'),
            ('assistant_capability_reject_event_mutation', '', 'assistant'),
            ('assistant_capability_reject_terminal_mutation', '', 'assistant'),
            ('assistant_runtime_reject_assignment_update', '', 'assistant'),
            ('assistant_runtime_reject_immutable_mutation', '', 'assistant'),
            ('claim_local_node_dispatch', 'uuid, character varying, character varying, character varying, uuid, character varying', 'gateway'),
            ('cleanup_old_usage_records', 'integer', 'gateway'),
            ('complete_assistant_runtime_model_call', 'uuid, bigint, bigint, bigint, character varying', 'assistant'),
            ('dispatch_assistant_capability_execution', 'uuid, character varying, character varying, character varying, uuid, bigint', 'assistant'),
            ('ensure_assistant_runtime_thread', 'uuid, character varying, character varying, character varying, character varying', 'assistant'),
            ('guard_dataset_process_rules_immutable', '', 'knowledge'),
            ('import_assistant_legacy_session', 'uuid, character varying, character varying, character varying', 'assistant'),
            ('issue_assistant_runtime_turn', 'uuid, uuid, uuid, uuid, character varying, character varying, character varying, character varying, character varying, jsonb, character, bigint, character varying, character varying, character varying, character varying, character varying, character, integer, bigint, bigint, bigint, timestamp with time zone, text', 'assistant'),
            ('prune_kb_document_progress_events', '', 'knowledge'),
            ('record_artifact_share_quiz_attempt', 'character varying, character, character varying, uuid, uuid, jsonb, double precision, integer, integer, character varying', 'gateway'),
            ('record_kb_document_progress_event', '', 'knowledge'),
            ('reject_mcp_snapshot_mutation', '', 'gateway'),
            ('reserve_assistant_capability_execution', 'uuid, uuid, character varying, character varying, character varying, uuid, character varying, character varying, character varying, bigint, jsonb, character, character varying, character varying, character varying, uuid, character varying, text, jsonb', 'assistant'),
            ('reserve_assistant_runtime_model_call', 'uuid, uuid, character, bigint, bigint, bigint', 'assistant'),
            ('segments_text_search_update', '', 'knowledge'),
            ('update_agent_traces_timestamp', '', 'gateway'),
            ('update_assistant_gateway_timestamp', '', 'assistant'),
            ('update_assistant_memory_chunks_text_search', '', 'assistant'),
            ('update_document_summaries_timestamp', '', 'knowledge'),
            ('update_eval_timestamp', '', 'gateway'),
            ('update_memory_timestamp', '', 'assistant'),
            ('update_segment_image_counts', '', 'knowledge'),
            ('update_updated_at_column', '', 'gateway')
        ) AS mapping(name, identity_arguments, target_schema)
        ORDER BY name, identity_arguments
    LOOP
        SELECT count(*), min(namespace.nspname)
        INTO matches, source_schema
        FROM pg_proc AS procedure
        JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND procedure.proname = desired.name
          -- Policy identities are type-only signatures suitable for ALTER /
          -- GRANT syntax. The catalog identity formatter includes input names
          -- when declared, so compare the canonical input type vector instead.
          AND oidvectortypes(procedure.proargtypes) = desired.identity_arguments
          AND procedure.prokind = 'f'
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_proc'::regclass
                AND dependency.objid = procedure.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation function %(%) matched % objects',
                desired.name, desired.identity_arguments, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER FUNCTION %I.%I(%s) SET SCHEMA %I',
                source_schema, desired.name, desired.identity_arguments,
                desired.target_schema
            );
        END IF;
    END LOOP;

    FOR desired IN
        SELECT column1 AS name, column2 AS target_schema FROM (
            VALUES (NULL::text, NULL::text, NULL::text)
        ) AS mapping
        WHERE column1 IS NOT NULL
        ORDER BY name
    LOOP
        SELECT count(*), min(namespace.nspname)
        INTO matches, source_schema
        FROM pg_type AS type_object
        JOIN pg_namespace AS namespace ON namespace.oid = type_object.typnamespace
        LEFT JOIN pg_class AS relation ON relation.oid = type_object.typrelid
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND type_object.typname = desired.name
          AND (type_object.typtype IN ('e', 'd', 'r', 'm')
               OR (type_object.typtype = 'c' AND relation.relkind = 'c'))
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_type'::regclass
                AND dependency.objid = type_object.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation type % matched % objects', desired.name, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER TYPE %I.%I SET SCHEMA %I',
                source_schema, desired.name, desired.target_schema
            );
        END IF;
    END LOOP;
END
$$;
-- END ARC03 GENERATED OBJECT RELOCATION

-- 2. Object ownership convergence -------------------------------------------
-- Every relation (table, view, materialized view, sequence, foreign table)
-- in the platform schemas becomes owned by the NOLOGIN owner.  The migrator
-- never owns business objects permanently; it SET ROLEs when DDL is needed.
DO $$
DECLARE
    target CONSTANT TEXT := 'ai_gateway_owner';
    r RECORD;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = target) THEN
        RAISE EXCEPTION
            'convergence refused: role % does not exist; run bootstrap/roles.sql first',
            target;
    END IF;

    FOR r IN
        SELECT n.nspname AS schema, c.relname AS name, c.relkind AS kind,
               pg_get_userbyid(c.relowner) AS current_owner
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND c.relkind IN ('r', 'p', 'm', 'S', 'v', 'f')
          AND NOT EXISTS (
              SELECT 1
              FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_class'::regclass
                AND dependency.objid = c.oid
                AND dependency.deptype = 'e'
          )
        ORDER BY n.nspname, c.relname
    LOOP
        IF r.current_owner <> target THEN
            EXECUTE format(
                'ALTER %s %I.%I OWNER TO %I',
                CASE r.kind
                    WHEN 'S' THEN 'SEQUENCE'
                    WHEN 'v' THEN 'VIEW'
                    WHEN 'm' THEN 'MATERIALIZED VIEW'
                    WHEN 'f' THEN 'FOREIGN TABLE'
                    ELSE 'TABLE'
                END,
                r.schema, r.name, target
            );
        END IF;
    END LOOP;

    -- Extension-owned objects are excluded: their owner/version are governed
    -- by the separate extension fingerprint and cannot be altered piecemeal.
    -- User functions, procedures and aggregates follow the platform owner.
    FOR r IN
        SELECT p.oid::regprocedure AS signature,
               pg_get_userbyid(p.proowner) AS current_owner,
               CASE p.prokind
                   WHEN 'p' THEN 'PROCEDURE'
                   WHEN 'a' THEN 'AGGREGATE'
                   ELSE 'FUNCTION'
               END AS object_kind
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND NOT EXISTS (
              SELECT 1
              FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_proc'::regclass
                AND dependency.objid = p.oid
                AND dependency.deptype = 'e'
          )
        ORDER BY p.oid::regprocedure::text
    LOOP
        IF r.current_owner <> target THEN
            EXECUTE format(
                'ALTER %s %s OWNER TO %I', r.object_kind, r.signature, target
            );
        END IF;
    END LOOP;

    FOR r IN
        SELECT n.nspname AS schema, t.typname AS name,
               pg_get_userbyid(t.typowner) AS current_owner
        FROM pg_type AS t
        JOIN pg_namespace AS n ON n.oid = t.typnamespace
        LEFT JOIN pg_class AS type_relation ON type_relation.oid = t.typrelid
        WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND (
              t.typtype IN ('e', 'd', 'r', 'm')
              OR (t.typtype = 'c' AND type_relation.relkind = 'c')
          )
          AND NOT EXISTS (
              SELECT 1
              FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_type'::regclass
                AND dependency.objid = t.oid
                AND dependency.deptype = 'e'
          )
        ORDER BY n.nspname, t.typname
    LOOP
        IF r.current_owner <> target THEN
            EXECUTE format('ALTER TYPE %I.%I OWNER TO %I', r.schema, r.name, target);
        END IF;
    END LOOP;

    -- Platform schemas themselves are owned by the owner so future DDL via
    -- SET ROLE works without granting CREATE to LOGIN roles.
    FOR r IN
        SELECT nspname AS schema, pg_get_userbyid(nspowner) AS current_owner
        FROM pg_namespace
        WHERE nspname IN ('public', 'gateway', 'assistant', 'knowledge')
    LOOP
        IF r.current_owner <> target THEN
            EXECUTE format('ALTER SCHEMA %I OWNER TO %I', r.schema, target);
        END IF;
    END LOOP;
END
$$;

-- 3. SECURITY DEFINER hardening ---------------------------------------------
-- Every SECURITY DEFINER routine is owned by the NOLOGIN owner and pinned to
-- pg_catalog plus its owner-controlled home schema.  public is safe here only
-- because schema ownership is already converged and PUBLIC CREATE is revoked.
DO $$
DECLARE
    target CONSTANT TEXT := 'ai_gateway_owner';
    f RECORD;
BEGIN
    FOR f IN
        SELECT p.oid::regprocedure AS signature,
               n.nspname AS schema,
               CASE p.prokind WHEN 'p' THEN 'PROCEDURE' ELSE 'FUNCTION' END AS object_kind
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND p.prosecdef
          AND p.prokind IN ('f', 'p', 'w')
          AND NOT EXISTS (
              SELECT 1
              FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_proc'::regclass
                AND dependency.objid = p.oid
                AND dependency.deptype = 'e'
          )
        ORDER BY p.oid::regprocedure::text
    LOOP
        EXECUTE format(
            'ALTER %s %s OWNER TO %I', f.object_kind, f.signature, target
        );
        EXECUTE format(
            'ALTER %s %s SET search_path = pg_catalog, %I',
            f.object_kind, f.signature, f.schema
        );
        EXECUTE format('REVOKE EXECUTE ON %s %s FROM PUBLIC', f.object_kind, f.signature);
    END LOOP;
END
$$;

-- 4. REVOKE PUBLIC ------------------------------------------------------------
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON SCHEMA gateway FROM PUBLIC;
REVOKE ALL ON SCHEMA assistant FROM PUBLIC;
REVOKE ALL ON SCHEMA knowledge FROM PUBLIC;

DO $$
DECLARE
    s TEXT;
    platform_routine RECORD;
    platform_type RECORD;
BEGIN
    FOREACH s IN ARRAY ARRAY['public', 'gateway', 'assistant', 'knowledge']
    LOOP
        EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA %I FROM PUBLIC', s);
        EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA %I FROM PUBLIC', s);
        -- Trusted extension members retain their bootstrap-admin owner and
        -- are hardened by the explicit admin prepare phase. The migrator may
        -- only modify owner-controlled platform routines here.
        FOR platform_routine IN
            SELECT p.oid::regprocedure AS signature,
                   CASE p.prokind
                       WHEN 'p' THEN 'PROCEDURE'
                       WHEN 'a' THEN 'AGGREGATE'
                       ELSE 'FUNCTION'
                   END AS object_kind
            FROM pg_proc AS p
            WHERE p.pronamespace = to_regnamespace(s)
              AND NOT EXISTS (
                  SELECT 1
                  FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_proc'::regclass
                    AND dependency.objid = p.oid
                    AND dependency.deptype = 'e'
              )
            ORDER BY p.oid::regprocedure::text
        LOOP
            EXECUTE format(
                'REVOKE EXECUTE ON %s %s FROM PUBLIC',
                platform_routine.object_kind,
                platform_routine.signature
            );
        END LOOP;
        FOR platform_type IN
            SELECT t.typname
            FROM pg_type AS t
            LEFT JOIN pg_class AS type_relation ON type_relation.oid = t.typrelid
            WHERE t.typnamespace = to_regnamespace(s)
              AND (
                  t.typtype IN ('e', 'd', 'r', 'm')
                  OR (t.typtype = 'c' AND type_relation.relkind = 'c')
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_type'::regclass
                    AND dependency.objid = t.oid
                    AND dependency.deptype = 'e'
              )
        LOOP
            EXECUTE format(
                'REVOKE USAGE ON TYPE %I.%I FROM PUBLIC', s, platform_type.typname
            );
        END LOOP;
    END LOOP;
END
$$;

-- Future objects created by the owner must not inherit PUBLIC grants.
-- Routine/type PUBLIC privileges originate in PostgreSQL's global defaults;
-- revoke those before applying the per-schema defaults below.
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE USAGE ON TYPES FROM PUBLIC;
