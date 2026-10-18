-- INERT, INDEPENDENT UNNUMBERED COMPLETE KNOWLEDGE/ASSISTANT RLS PROPOSAL.
-- Supersedes the unactivated nine-table tenant-rls.sql candidate; do not apply both.
-- Gateway rollout remains the next measured phase. Application DDL remains authority-owned.
-- Requires tenant-scoped foreground AND background transactions before activation.
-- Rollback class: old-binary-compatible only while every supported binary binds context;
-- otherwise coordinated application rollback plus authority-reviewed policy rollback.
SET LOCAL lock_timeout = '2s';
SET LOCAL statement_timeout = '60s';
DO $roles$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname IN
    ('ai_gateway_gateway','ai_gateway_runtime','ai_gateway_capability_worker',
     'ai_gateway_knowledge_api','ai_gateway_knowledge_worker') AND (rolsuper OR rolbypassrls OR EXISTS (
       SELECT 1 FROM pg_catalog.pg_roles privileged WHERE (privileged.rolsuper OR privileged.rolbypassrls)
       AND pg_catalog.pg_has_role(pg_roles.rolname,privileged.rolname,'MEMBER')))) THEN
    RAISE EXCEPTION 'application role bypasses row security';
  END IF;
END $roles$;

-- Narrow service control reads remain available before a tenant transaction can
-- be established. Bound calls stay limited to that tenant; other roles cannot
-- invoke these entrypoints. No application receives BYPASSRLS or owner membership.
CREATE FUNCTION assistant.runtime_thread_scope_v1(p_thread uuid,p_root boolean,p_include_deleted boolean)
RETURNS TABLE(runtime_thread_id uuid,tenant_id text,user_id text,session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $scope$
  SELECT r.runtime_thread_id,r.tenant_id::text,r.user_id::text,r.session_id::text
  FROM assistant.assistant_runtime_threads r
  WHERE p_root AND r.runtime_thread_id=p_thread AND (p_include_deleted OR r.deleted_at IS NULL)
    AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR r.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
  UNION ALL
  SELECT m.runtime_thread_id,m.tenant_id::text,m.user_id::text,m.session_id::text
  FROM assistant.assistant_runtime_thread_members m
  JOIN assistant.assistant_runtime_threads r ON r.runtime_thread_id=m.runtime_thread_id
  WHERE NOT p_root AND m.kernel_thread_id=p_thread AND (p_include_deleted OR r.deleted_at IS NULL)
    AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR m.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$scope$;
ALTER FUNCTION assistant.runtime_thread_scope_v1(uuid,boolean,boolean) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.runtime_thread_scope_v1(uuid,boolean,boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.runtime_thread_scope_v1(uuid,boolean,boolean) TO ai_gateway_runtime;

CREATE FUNCTION assistant.runtime_run_scope_v1(p_run uuid)
RETURNS TABLE(runtime_thread_id uuid,tenant_id text,user_id text,session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $run_scope$
  SELECT s.runtime_thread_id,r.tenant_id::text,r.user_id::text,r.session_id::text
  FROM assistant.assistant_runs r
  JOIN assistant.assistant_runtime_snapshots s ON s.snapshot_id=r.runtime_snapshot_id
    AND s.run_id=r.run_id AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id
  WHERE r.run_id=p_run AND r.engine='agent_runtime'
    AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR r.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$run_scope$;
ALTER FUNCTION assistant.runtime_run_scope_v1(uuid) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.runtime_run_scope_v1(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.runtime_run_scope_v1(uuid) TO ai_gateway_runtime;

CREATE FUNCTION assistant.recoverable_capability_executions_v1()
RETURNS SETOF assistant.assistant_capability_executions
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $recovery$
SELECT e.*
FROM assistant_capability_executions AS e
WHERE (
    (
        (
            (e.effect = 'read' AND e.status IN ('published', 'dispatched', 'running'))
            OR (
                e.effect IN ('write', 'unknown')
                AND e.status = 'published'
                AND e.dispatch_fence IS NULL
            )
        )
        AND (EXISTS (
            SELECT 1
            FROM assistant_runs AS r
            JOIN assistant_runtime_model_leases AS l
              ON l.run_id = r.run_id
             AND l.tenant_id = r.tenant_id
             AND l.user_id = r.user_id
             AND l.session_id = r.session_id
            JOIN assistant_runtime_snapshots AS s
              ON s.snapshot_id = l.snapshot_id
             AND s.run_id = r.run_id
             AND s.tenant_id = r.tenant_id
             AND s.user_id = r.user_id
             AND s.session_id = r.session_id
            LEFT JOIN assistant_runtime_snapshot_revocations AS x
              ON x.snapshot_id = s.snapshot_id
             AND x.tenant_id = s.tenant_id
             AND x.user_id = s.user_id
             AND x.session_id = s.session_id
            WHERE r.run_id = e.run_id
              AND r.tenant_id = e.tenant_id
              AND r.user_id = e.user_id
              AND r.session_id = e.session_id
              AND r.status = 'running'
              AND r.engine = 'agent_runtime'
              AND l.status = 'active'
              AND l.expires_at > NOW()
              AND (s.expires_at IS NULL OR s.expires_at > NOW())
              AND x.snapshot_id IS NULL
        ) OR EXISTS (
            SELECT 1 FROM assistant.assistant_sdk_runs AS s
            JOIN assistant_runs AS r ON r.run_id=s.run_id
              AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id
            WHERE s.run_id=e.run_id AND s.tenant_id=e.tenant_id
              AND s.user_id=e.user_id AND s.session_id=e.session_id
              AND s.engine IN ('openai_agents','claude_agent_sdk')
              AND r.engine=s.engine AND r.status='running'
              AND r.harness_thread_id IS NULL AND r.harness_turn_id IS NULL
              AND s.revoked_at IS NULL AND s.expires_at > clock_timestamp()
              AND s.lease_until > clock_timestamp() AND s.fence > 0
              AND s.capability_revision=e.capability_revision
              AND s.snapshot_id::text=e.resource_binding->>'snapshot_id'
        ))
    )
    OR (
        e.effect IN ('write', 'unknown')
        AND e.status IN ('dispatched', 'running')
        AND e.dispatch_fence IS NOT NULL
        AND (e.worker_lease_until IS NULL OR e.worker_lease_until <= NOW())
    )
)
AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL
  OR e.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
ORDER BY e.created_at
LIMIT 256
$recovery$;
ALTER FUNCTION assistant.recoverable_capability_executions_v1() OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.recoverable_capability_executions_v1() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.recoverable_capability_executions_v1() TO ai_gateway_capability_worker;

CREATE FUNCTION knowledge.discover_pipeline_work_v1() RETURNS bigint
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $discover$
DECLARE affected bigint;
BEGIN
  INSERT INTO knowledge.pipeline_work(document_id,dataset_id,tenant_id,job_id,phase,status,execution_id,checkpoints) SELECT d.document_id,d.dataset_id,ds.tenant_id,gen_random_uuid(),'parse','ready',d.metadata->>'_document_pipeline_execution_id','{}'::jsonb FROM knowledge.documents d JOIN knowledge.datasets ds USING(dataset_id) WHERE d.status='waiting' AND NOT ds.is_deleted AND NOT ds.is_archived AND COALESCE(d.enabled,TRUE) AND NOT COALESCE(d.archived,FALSE) AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR ds.tenant_id=pg_catalog.current_setting('app.tenant_id',true)) ORDER BY d.created_at LIMIT 1000 ON CONFLICT(document_id) DO UPDATE SET job_id=EXCLUDED.job_id,phase=EXCLUDED.phase,status='ready',execution_id=EXCLUDED.execution_id,checkpoints='{}'::jsonb,owner_token=NULL,lease_expires_at=NULL,fence=knowledge.pipeline_work.fence+1 WHERE knowledge.pipeline_work.status IN ('completed','error');
  GET DIAGNOSTICS affected = ROW_COUNT;
  RETURN affected;
END
$discover$;
ALTER FUNCTION knowledge.discover_pipeline_work_v1() OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.discover_pipeline_work_v1() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.discover_pipeline_work_v1() TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.claim_pipeline_work_v1(p_phase text,p_token text,p_lease_seconds double precision)
RETURNS SETOF jsonb LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $claim$
WITH candidate AS (SELECT w.document_id FROM knowledge.pipeline_work w JOIN knowledge.documents d ON d.document_id=w.document_id AND d.dataset_id=w.dataset_id JOIN knowledge.datasets ds ON ds.dataset_id=w.dataset_id AND ds.tenant_id=w.tenant_id WHERE (w.phase=$1 OR $1='all') AND (w.status='ready' OR (w.status='running' AND w.lease_expires_at<=clock_timestamp())) AND NOT ds.is_deleted AND NOT ds.is_archived AND d.status IN ('waiting','parsing','splitting','indexing') AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR w.tenant_id=pg_catalog.current_setting('app.tenant_id',true)) ORDER BY w.updated_at,w.document_id FOR UPDATE OF w SKIP LOCKED LIMIT 1) UPDATE knowledge.pipeline_work w SET status='running',owner_token=$2,fence=w.fence+1,lease_expires_at=clock_timestamp()+make_interval(secs=>$3),updated_at=clock_timestamp() FROM candidate c WHERE w.document_id=c.document_id RETURNING to_jsonb(w)
$claim$;
ALTER FUNCTION knowledge.claim_pipeline_work_v1(text,text,double precision) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.claim_pipeline_work_v1(text,text,double precision) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.claim_pipeline_work_v1(text,text,double precision) TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.document_ingest_scope_v1(p_dataset text,p_document text)
RETURNS TABLE(tenant_id text,user_id text,execution_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $ingest_scope$
  SELECT ds.tenant_id::text,COALESCE(e.triggered_by,d.created_by)::text,
    NULLIF(d.metadata->>'_document_pipeline_execution_id','')
  FROM knowledge.documents d
  JOIN knowledge.datasets ds ON ds.dataset_id=d.dataset_id
  LEFT JOIN knowledge.document_pipeline_executions e
    ON e.execution_id=d.metadata->>'_document_pipeline_execution_id'
    AND e.dataset_id=d.dataset_id AND e.document_id=d.document_id AND e.status='running'
  WHERE d.dataset_id=p_dataset AND d.document_id=p_document
    AND NOT ds.is_deleted AND NOT ds.is_archived
    AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL
      OR ds.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$ingest_scope$;
ALTER FUNCTION knowledge.document_ingest_scope_v1(text,text) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.document_ingest_scope_v1(text,text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.document_ingest_scope_v1(text,text) TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.queued_documents_v1(p_limit bigint,p_cursor text,p_priority_seconds double precision,p_actions text[])
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $queue$ SELECT to_jsonb(q) FROM (WITH eligible AS (
                    SELECT
                        ds.tenant_id,
                        d.dataset_id,
                        d.document_id,
                        d.updated_at,
                        CASE
                            WHEN COALESCE(d.metadata, '{}'::jsonb)
                                ->> '_document_ingest_action'
                                = ANY($4::text[])
                            THEN 'interactive'
                            ELSE 'bulk'
                        END AS dispatch_lane,
                        d.updated_at - (
                            CASE
                                WHEN COALESCE(d.metadata, '{}'::jsonb)
                                    ->> '_document_ingest_action'
                                    = ANY($4::text[])
                                THEN $3::double precision
                                ELSE 0
                            END * INTERVAL '1 second'
                        ) AS dispatch_order_at
                    FROM knowledge.documents AS d
                    JOIN knowledge.datasets AS ds ON ds.dataset_id = d.dataset_id
                    WHERE d.status = 'waiting' AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR ds.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
                      AND ds.is_deleted = FALSE
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_document_upload_generation'
                      )
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_document_upload_failed'
                      )
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_confluence_sync_generation'
                      )
                      AND NOT COALESCE(
                            COALESCE(ds.index_config, '{}'::jsonb)
                                -> 'retrieval' ? '_index_deletion_fence',
                            FALSE
                      )
                ), tenant_ranked AS (
                    SELECT
                        eligible.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY tenant_id
                            ORDER BY dispatch_order_at, updated_at, document_id
                        ) AS tenant_round
                    FROM eligible
                )
                SELECT
                    tenant_id,
                    dataset_id,
                    document_id,
                    dispatch_lane
                FROM tenant_ranked
                ORDER BY
                    tenant_round,
                    CASE
                        WHEN $2::text = '' OR tenant_id > $2::text THEN 0
                        ELSE 1
                    END,
                    tenant_id,
                    dispatch_order_at,
                    updated_at,
                    document_id
                LIMIT LEAST(GREATEST($1,1),1000)) q $queue$;
ALTER FUNCTION knowledge.queued_documents_v1(bigint,text,double precision,text[]) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.queued_documents_v1(bigint,text,double precision,text[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.queued_documents_v1(bigint,text,double precision,text[]) TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.claim_document_batch_item_v1(p_worker text,p_stale_seconds double precision)
RETURNS jsonb LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $batch$
DECLARE selected_op knowledge.kb_document_batch_operations%ROWTYPE; picked_document text;
BEGIN
  IF p_worker IS NULL OR length(p_worker)=0 OR p_stale_seconds < 1 OR p_stale_seconds IS NULL THEN
    RAISE EXCEPTION 'invalid document batch claim' USING ERRCODE='22023';
  END IF;
  SELECT op.* INTO selected_op
                    FROM knowledge.kb_document_batch_operations AS op
                    WHERE op.status IN ('pending', 'running') AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR op.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
                      AND EXISTS (
                          SELECT 1
                          FROM knowledge.kb_document_batch_items AS item
                          WHERE item.operation_id = op.operation_id
                            AND (
                                item.status = 'pending'
                                OR (
                                    item.status = 'claiming'
                                    AND item.claimed_at < NOW()
                                        - make_interval(secs => p_stale_seconds)
                                )
                            )
                      )
                    ORDER BY op.last_claimed_at NULLS FIRST, op.created_at,
                             op.operation_id
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  SELECT document_id INTO picked_document
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = selected_op.operation_id::uuid
                      AND (
                          status = 'pending'
                          OR (
                              status = 'claiming'
                              AND claimed_at < NOW()
                                  - make_interval(secs => p_stale_seconds)
                          )
                      )
                    ORDER BY ordinal
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  UPDATE knowledge.kb_document_batch_items
                    SET status = 'claiming', claimed_by = p_worker,
                        claimed_at = NOW(), error_code = NULL, error = NULL
                    WHERE operation_id = selected_op.operation_id::uuid AND document_id = picked_document;
  UPDATE knowledge.kb_document_batch_operations
                    SET status = 'running', last_claimed_at = NOW(),
                        updated_at = NOW()
                    WHERE operation_id = selected_op.operation_id::uuid;
  RETURN jsonb_build_object('operation_id',selected_op.operation_id::text,'tenant_id',selected_op.tenant_id,
    'dataset_id',selected_op.dataset_id,'operation',selected_op.operation,'document_id',picked_document,
    'created_by',selected_op.created_by,'actor_roles',COALESCE(selected_op.actor_roles,'[]'::jsonb));
END $batch$;
ALTER FUNCTION knowledge.claim_document_batch_item_v1(text,double precision) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.claim_document_batch_item_v1(text,double precision) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.claim_document_batch_item_v1(text,double precision) TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.stuck_documents_v1(p_threshold_minutes integer)
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $recover$ SELECT to_jsonb(d)||jsonb_build_object('tenant_id',ds.tenant_id) FROM knowledge.documents d JOIN knowledge.datasets ds USING(dataset_id) WHERE (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR ds.tenant_id=pg_catalog.current_setting('app.tenant_id',true)) AND d.status IN ('parsing','splitting','indexing') AND d.updated_at<NOW()-(GREATEST($1,1)*INTERVAL '1 minute') AND ds.is_deleted=FALSE AND ds.is_archived=FALSE ORDER BY d.updated_at LIMIT 100 $recover$;
ALTER FUNCTION knowledge.stuck_documents_v1(integer) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.stuck_documents_v1(integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.stuck_documents_v1(integer) TO ai_gateway_knowledge_worker;

CREATE FUNCTION knowledge.unfinished_publications_v1()
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $recover$ SELECT to_jsonb(ds) FROM knowledge.datasets ds WHERE (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR ds.tenant_id=pg_catalog.current_setting('app.tenant_id',true)) AND ds.is_deleted=FALSE AND ds.content_revision<0 AND EXISTS(SELECT 1 FROM knowledge.document_pipeline_executions e WHERE e.dataset_id=ds.dataset_id AND e.status='running' AND e.manifest?'special_publication') LIMIT 100 $recover$;
ALTER FUNCTION knowledge.unfinished_publications_v1() OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.unfinished_publications_v1() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.unfinished_publications_v1() TO ai_gateway_knowledge_worker;

CREATE FUNCTION assistant.gateway_run_scope_v1(p_run uuid)
RETURNS TABLE(tenant_id text,user_id text,session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $gateway_run$
  SELECT r.tenant_id::text,r.user_id::text,r.session_id::text
  FROM assistant.assistant_runs r WHERE r.run_id=p_run
    AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL
      OR r.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$gateway_run$;
ALTER FUNCTION assistant.gateway_run_scope_v1(uuid) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.gateway_run_scope_v1(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.gateway_run_scope_v1(uuid) TO ai_gateway_gateway;

CREATE FUNCTION assistant.gateway_model_lease_scope_v1(p_id uuid)
RETURNS TABLE(tenant_id text,user_id text,session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $ownership$
  SELECT r.tenant_id::text,r.user_id::text,r.session_id::text FROM assistant.assistant_runtime_model_leases r
  WHERE r.lease_id=p_id AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL
    OR r.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$ownership$;
ALTER FUNCTION assistant.gateway_model_lease_scope_v1(uuid) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.gateway_model_lease_scope_v1(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.gateway_model_lease_scope_v1(uuid) TO ai_gateway_gateway;

CREATE FUNCTION assistant.gateway_model_call_scope_v1(p_id uuid)
RETURNS TABLE(tenant_id text,user_id text,session_id text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp
AS $ownership$
  SELECT r.tenant_id::text,r.user_id::text,r.session_id::text FROM assistant.assistant_runtime_model_calls r
  WHERE r.call_id=p_id AND (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL
    OR r.tenant_id=pg_catalog.current_setting('app.tenant_id',true))
$ownership$;
ALTER FUNCTION assistant.gateway_model_call_scope_v1(uuid) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION assistant.gateway_model_call_scope_v1(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION assistant.gateway_model_call_scope_v1(uuid) TO ai_gateway_gateway;

CREATE FUNCTION knowledge.next_confluence_acl_refresh_v1()
RETURNS SETOF jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,knowledge,pg_temp
AS $acl_refresh$ SELECT jsonb_build_object('binding',b.binding_id,'dataset',b.dataset_id,'tenant',b.tenant_id,'actor',b.created_by,'page',p.page_id) FROM confluence_pages p JOIN confluence_space_bindings b ON b.binding_id=p.binding_id JOIN confluence_connections c ON c.connection_id=b.connection_id JOIN knowledge.documents d ON d.document_id=p.document_id AND d.dataset_id=b.dataset_id WHERE (NULLIF(pg_catalog.current_setting('app.tenant_id',true),'') IS NULL OR b.tenant_id=pg_catalog.current_setting('app.tenant_id',true)) AND b.status<>'syncing' AND b.sync_enabled IS DISTINCT FROM FALSE AND c.status='active' AND CASE WHEN COALESCE(d.metadata->'_connector_acl'->>'expires_at','') ~ '^[0-9]{1,12}$' THEN (d.metadata->'_connector_acl'->>'expires_at')::bigint ELSE 0 END < extract(epoch from clock_timestamp())+900 ORDER BY d.updated_at,p.page_id LIMIT 1 $acl_refresh$;
ALTER FUNCTION knowledge.next_confluence_acl_refresh_v1() OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION knowledge.next_confluence_acl_refresh_v1() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION knowledge.next_confluence_acl_refresh_v1() TO ai_gateway_knowledge_api;

DO $policies$
DECLARE target text; predicate text;
BEGIN
  FOREACH target IN ARRAY ARRAY[
    'assistant.agent_runtime_attachments',
    'assistant.agent_runtime_feedback',
    'assistant.agent_runtime_idempotency',
    'assistant.artifact_shares',
    'assistant.artifacts',
    'assistant.assistant_audit_events',
    'assistant.assistant_capability_events',
    'assistant.assistant_capability_executions',
    'assistant.assistant_command_queue',
    'assistant.assistant_context_breakdown',
    'assistant.assistant_memory_chunks',
    'assistant.assistant_memory_reflections',
    'assistant.assistant_memory_sources',
    'assistant.assistant_run_checkpoints',
    'assistant.assistant_runs',
    'assistant.assistant_runtime_items',
    'assistant.assistant_runtime_model_calls',
    'assistant.assistant_runtime_model_leases',
    'assistant.assistant_runtime_snapshot_revocations',
    'assistant.assistant_runtime_snapshots',
    'assistant.assistant_runtime_thread_members',
    'assistant.assistant_runtime_thread_projections',
    'assistant.assistant_runtime_threads',
    'assistant.assistant_scheduler_jobs',
    'assistant.assistant_session_runtime_assignments',
    'assistant.assistant_skill_runs',
    'assistant.assistant_skill_version_revocations',
    'assistant.assistant_skill_versions',
    'assistant.assistant_skills',
    'assistant.assistant_tool_approvals',
    'assistant.conversation_shares',
    'assistant.exams',
    'assistant.image_tasks',
    'assistant.quizzes',
    'assistant.session_memory',
    'assistant.sessions',
    'assistant.user_memory',
    'knowledge.child_chunks',
    'knowledge.confluence_connections',
    'knowledge.confluence_space_bindings',
    'knowledge.dataset_collection_bindings',
    'knowledge.dataset_query_feedback',
    'knowledge.datasets',
    'knowledge.kb_bm25_v2_lifecycle',
    'knowledge.kb_document_batch_operations',
    'knowledge.kb_parsing_ir',
    'knowledge.kb_parsing_page_cache',
    'knowledge.kb_segment_attachment_bindings'
  ] LOOP
    IF (SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid=target::regclass) <> 'ai_gateway_owner' THEN RAISE EXCEPTION 'RLS target owner mismatch: %',target; END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=target::regclass AND attname='tenant_id' AND NOT attisdropped AND atttypid IN ('text'::regtype,'varchar'::regtype,'bpchar'::regtype)) THEN RAISE EXCEPTION 'RLS tenant column contract mismatch: %',target; END IF;
    -- current_setting(...,true) returns NULL when unbound; empty/legacy scope is never global.
    predicate := 'tenant_id = NULLIF(pg_catalog.current_setting(''app.tenant_id'',true),'''')';
    EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY',target);
    EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY',target);
    EXECUTE format('CREATE POLICY tenant_allow ON %s AS PERMISSIVE FOR ALL TO ai_gateway_gateway,ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker USING (%s) WITH CHECK (%s)',target,predicate,predicate);
    -- A future permissive policy must not accidentally broaden tenant access.
    EXECUTE format('CREATE POLICY tenant_guard ON %s AS RESTRICTIVE FOR ALL TO ai_gateway_gateway,ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker USING (%s) WITH CHECK (%s)',target,predicate,predicate);
    -- The NOLOGIN authority owner retains its preexisting maintenance rights.
    -- Applications must never be members of this role; no membership is granted here.
    EXECUTE format('CREATE POLICY authority_maintenance ON %s FOR ALL TO ai_gateway_owner USING (true) WITH CHECK (true)',target);
  END LOOP;
  FOREACH target IN ARRAY ARRAY['knowledge.documents','knowledge.segments'] LOOP
    -- These actual baseline tables have dataset_id, not tenant_id.
    predicate := format('EXISTS (SELECT 1 FROM knowledge.datasets AS tenant_dataset WHERE tenant_dataset.dataset_id=%s.dataset_id AND tenant_dataset.tenant_id=NULLIF(pg_catalog.current_setting(''app.tenant_id'',true),''''))',split_part(target,'.',2));
    EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY',target);
    EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY',target);
    EXECUTE format('CREATE POLICY tenant_allow ON %s AS PERMISSIVE FOR ALL TO ai_gateway_gateway,ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker USING (%s) WITH CHECK (%s)',target,predicate,predicate);
    EXECUTE format('CREATE POLICY tenant_guard ON %s AS RESTRICTIVE FOR ALL TO ai_gateway_gateway,ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker USING (%s) WITH CHECK (%s)',target,predicate,predicate);
    EXECUTE format('CREATE POLICY authority_maintenance ON %s FOR ALL TO ai_gateway_owner USING (true) WITH CHECK (true)',target);
  END LOOP;
END $policies$;
DO $ownership$
DECLARE application_role text;
BEGIN
  FOREACH application_role IN ARRAY ARRAY['ai_gateway_gateway','ai_gateway_runtime',
    'ai_gateway_capability_worker','ai_gateway_knowledge_api','ai_gateway_knowledge_worker'] LOOP
    IF pg_catalog.pg_has_role(application_role,'ai_gateway_owner','MEMBER') THEN
      RAISE EXCEPTION 'application must not inherit authority maintenance identity';
    END IF;
  END LOOP;
END $ownership$;
