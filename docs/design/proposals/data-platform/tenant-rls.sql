-- INERT, UNNUMBERED AUTHORITY PROPOSAL. Never run as application startup SQL.
-- Requires tenant-scoped foreground AND background transactions before activation.
-- Rollback class: old-binary-compatible only while every supported binary binds context;
-- otherwise coordinated application rollback plus authority-reviewed policy rollback.
SET LOCAL lock_timeout = '2s';
SET LOCAL statement_timeout = '60s';
DO $roles$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname IN
    ('ai_gateway_gateway','ai_gateway_runtime','ai_gateway_capability_worker',
     'ai_gateway_knowledge_api','ai_gateway_knowledge_worker') AND (rolsuper OR rolbypassrls)) THEN
    RAISE EXCEPTION 'application role bypasses row security';
  END IF;
END $roles$;

DO $policies$
DECLARE target text; predicate text;
BEGIN
  FOREACH target IN ARRAY ARRAY[
    'knowledge.datasets','knowledge.child_chunks','knowledge.confluence_connections',
    'assistant.artifacts','assistant.assistant_runtime_items','assistant.assistant_runs',
    'assistant.sessions'
  ] LOOP
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
