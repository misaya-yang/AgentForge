-- Inactive035: install six complete Data inputs; all22 families remain registered. No concurrent DDL, expansion, cutover or retention is invoked.
DO $service_owner$ BEGIN IF (current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')) IS NOT TRUE THEN RAISE EXCEPTION 'canonical scoped schema owner required'; END IF; END $service_owner$;
-- INERT, INDEPENDENT, UNNUMBERED AUTHORITY SUCCESSOR PROPOSAL. Supersedes the
-- audit-only candidate after authority admission; never apply both expansions.
-- No <=029 migration or frozen authority fingerprint is modified.
-- Native runner enforces the original2s lock/60s statement budget.
CREATE SCHEMA partition_ops AUTHORIZATION CURRENT_USER;
REVOKE ALL ON SCHEMA partition_ops FROM PUBLIC;
CREATE TABLE partition_ops.families (
 family text PRIMARY KEY, clock_column name NOT NULL, cadence text NOT NULL CHECK(cadence IN('month','day')),
 safe_retention_predicate text NOT NULL, source_oid oid, parent_oid oid, registry_oid oid,
 boundary timestamptz, primary_columns name[], key_columns name[], indexes jsonb,
 cursor jsonb, phase text NOT NULL DEFAULT 'registered' CHECK(phase IN('registered','expanded','backfilled','validated','active')),
 revision bigint NOT NULL DEFAULT 0, admission_sha256 text, compatibility_sha256 text,
 CHECK(admission_sha256 IS NULL OR admission_sha256 ~ '^[0-9a-f]{64}$')
);
CREATE TABLE partition_ops.partitions (
 family text NOT NULL REFERENCES partition_ops.families(family), relation_oid oid NOT NULL UNIQUE,
 relation_name text NOT NULL UNIQUE, lower_at timestamptz, upper_at timestamptz NOT NULL,
 legacy boolean NOT NULL DEFAULT false, state text NOT NULL CHECK(state IN('active','detaching','detached','archived','dropped')),
 archive_receipt jsonb, retention_evidence jsonb, sealed_acl jsonb, archive_history jsonb NOT NULL DEFAULT '[]'::jsonb, PRIMARY KEY(family,relation_name)
);
CREATE TABLE partition_ops.foreign_keys (
 original_oid oid PRIMARY KEY, source_oid oid NOT NULL, target_family text NOT NULL REFERENCES partition_ops.families(family),
 original_name name NOT NULL, original_definition text NOT NULL, replacement_name name NOT NULL,
 source_columns name[] NOT NULL, target_columns name[] NOT NULL, phase text NOT NULL DEFAULT 'captured'
 CHECK(phase IN('captured','expanded','validated','rebound'))
);
-- State guards are deliberately conservative: unresolved model calls, unsettled
-- leases, active threads/executions and live traces are never retention victims.
INSERT INTO partition_ops.families(family,clock_column,cadence,safe_retention_predicate) VALUES
 ('gateway.usage_records','created_at','month',$p$COALESCE(t.status,'unknown') NOT IN ('running','pending','unknown')$p$),
 ('gateway.request_traces','created_at','day',$p$t.status NOT IN ('running','pending','unknown')$p$),
 ('gateway.audit_logs','created_at','month','true'),
 ('gateway.billing_events','event_time','month',$p$t.status NOT IN ('running','pending','reconciliation_required','unknown')$p$),
 ('gateway.agent_trace_spans','started_at','day',$p$t.status<>'running' AND EXISTS(SELECT 1 FROM gateway.agent_traces a WHERE a.trace_id=t.trace_id AND a.status<>'running')$p$),
 ('gateway.agent_trace_events','occurred_at','day',$p$EXISTS(SELECT 1 FROM gateway.agent_traces a WHERE a.trace_id=t.trace_id AND a.status<>'running')$p$),
 ('gateway.tool_audit_log','created_at','month','true'),
 ('gateway.login_audit','created_at','month','true'),
 ('gateway.service_health_records','checked_at','month','true'),
 ('assistant.assistant_runtime_items','created_at','day',$p$EXISTS(SELECT 1 FROM assistant.assistant_runtime_threads a WHERE a.runtime_thread_id=t.runtime_thread_id AND a.tenant_id=t.tenant_id AND a.user_id=t.user_id AND a.session_id=t.session_id AND (a.archived_at IS NOT NULL OR a.deleted_at IS NOT NULL))$p$),
 ('assistant.assistant_audit_events','created_at','month','true'),
 ('assistant.assistant_capability_events','created_at','day',$p$EXISTS(SELECT 1 FROM assistant.assistant_capability_executions a WHERE a.execution_id=t.execution_id AND a.tenant_id=t.tenant_id AND a.terminal_at IS NOT NULL AND a.status IN ('succeeded','failed','cancelled','timeout'))$p$),
 ('knowledge.kb_document_progress_events','created_at','month',$p$t.event_type='deleted' OR EXISTS(SELECT 1 FROM knowledge.documents d WHERE d.dataset_id=t.dataset_id AND d.document_id=t.document_id AND d.status IN ('completed','error','failed','disabled'))$p$),
 ('assistant.assistant_runtime_model_calls','reserved_at','month',$p$t.status IN ('completed','failed') AND t.completed_at IS NOT NULL$p$),
 ('assistant.assistant_runtime_model_leases','issued_at','month',$p$t.status IN ('exhausted','revoked','expired') AND t.expires_at<clock_timestamp() AND t.reserved_input_tokens=0 AND t.reserved_output_tokens=0 AND t.reserved_cost_microusd=0 AND t.calls_completed=t.calls_reserved$p$),
 ('gateway.knowledge_model_calls','issued_at','month',$p$t.state IN ('completed','rejected') AND t.completed_at IS NOT NULL AND t.result IS NOT NULL AND t.expires_at<clock_timestamp()$p$),
 ('gateway.task_reconciliation_audit','resolved_at','month','true'),
 ('gateway.price_book_v2_activation_audit','created_at','month','true'),
 ('gateway.price_book_v2_import_audit','created_at','month','true'),
 ('gateway.price_book_v2_usage_receipts','created_at','month','true'),
 ('gateway.price_book_v2_accounting_checkpoints','updated_at','month','true'),
 ('gateway.agent_traces','started_at','day',$p$t.status<>'running' AND (t.retention_expires_at IS NULL OR t.retention_expires_at<clock_timestamp())$p$);
-- No fallback to a one-table subset. Missing schema successors are a prerequisite
-- for their selected family, not a reason to silently drop it from the catalog.
CREATE FUNCTION partition_ops.assert_owner() RETURNS void LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
 OR EXISTS(SELECT 1 FROM pg_roles WHERE rolname=current_user AND (rolcanlogin OR rolsuper OR rolbypassrls OR rolcreatedb OR rolcreaterole OR rolreplication))
 OR (SELECT count(*) FROM pg_namespace WHERE nspname IN('gateway','assistant','knowledge') AND pg_get_userbyid(nspowner)=current_user)<>3
 THEN RAISE EXCEPTION 'native authority NOLOGIN schema owner required'; END IF;
END $$;
-- The family flag is authority state, never an environment switch. No app can
-- create/read the directory. Shared locks live through the caller transaction.
CREATE FUNCTION partition_ops.family_enabled(p_family text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; rel oid;
BEGIN
 PERFORM pg_advisory_xact_lock_shared(hashtextextended('agentforge.append.family:'||p_family,0));
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 rel:=COALESCE(CASE WHEN f.phase='active' THEN f.parent_oid ELSE f.source_oid END,to_regclass(p_family)::oid);
 IF rel IS NULL OR NOT (has_table_privilege(session_user,rel,'SELECT') OR has_table_privilege(session_user,rel,'INSERT') OR has_table_privilege(session_user,rel,'DELETE')) THEN RAISE EXCEPTION 'original family privilege required'; END IF;
 RETURN f.phase<>'registered';
END $$;
REVOKE ALL ON FUNCTION partition_ops.family_enabled(text) FROM PUBLIC;
CREATE FUNCTION partition_ops.identity(p_columns name[], p_row jsonb) RETURNS jsonb
LANGUAGE sql IMMUTABLE STRICT SET search_path=pg_catalog,pg_temp AS $$
 SELECT jsonb_object_agg(c,p_row->c) FROM unnest(p_columns) c
$$;
CREATE FUNCTION partition_ops.projection(p_columns name[],p_row jsonb) RETURNS jsonb
LANGUAGE sql IMMUTABLE STRICT SET search_path=pg_catalog,pg_temp AS $$
 SELECT COALESCE(jsonb_object_agg(c,p_row->c),'{}'::jsonb) FROM unnest(p_columns) c
$$;
CREATE FUNCTION partition_ops.claim_row(p_family text,p_row jsonb,p_identity jsonb,p_moving boolean DEFAULT false)
RETURNS void LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; keyrow jsonb; physical text; at_time timestamptz;
BEGIN
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 at_time:=COALESCE((p_row->>f.clock_column)::timestamptz,'-infinity'::timestamptz);
 SELECT relation_oid::text INTO physical FROM partition_ops.partitions WHERE family=p_family
 AND (lower_at IS NULL OR at_time>=lower_at) AND at_time<upper_at AND state='active' FOR KEY SHARE;
 IF physical IS NULL THEN RAISE EXCEPTION 'partition unavailable; no local/default fallback'; END IF;
 keyrow:=partition_ops.projection(f.key_columns,p_row)||jsonb_build_object('af_identity',p_identity,'af_time',at_time,'af_partition',physical,'af_moving_xid',CASE WHEN p_moving THEN txid_current() ELSE NULL END);
 EXECUTE format('INSERT INTO %s SELECT (jsonb_populate_record(NULL::%s,$1)).*',f.registry_oid::regclass,f.registry_oid::regclass) USING keyrow;
END $$;
-- A trigger-only definer, never an application-callable generic registry writer.
CREATE FUNCTION partition_ops.row_guard() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; old_key jsonb; new_key jsonb; old_time timestamptz; new_time timestamptz; moving bigint; move_from jsonb; exists_key boolean; physical text; patch jsonb; assignments text;
BEGIN
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=TG_ARGV[0];
 IF TG_LEVEL<>'ROW' OR TG_NARGS<>1 OR NOT (TG_RELID=f.source_oid OR TG_RELID=f.parent_oid OR EXISTS(SELECT 1 FROM pg_partition_ancestors(TG_RELID) a WHERE a.relid=f.parent_oid)) THEN RAISE EXCEPTION 'invalid global identity trigger'; END IF;
 IF TG_OP<>'INSERT' THEN old_key:=partition_ops.identity(f.primary_columns,to_jsonb(OLD)); old_time:=COALESCE((to_jsonb(OLD)->>f.clock_column)::timestamptz,'-infinity'::timestamptz); END IF;
 IF TG_OP<>'DELETE' THEN new_key:=partition_ops.identity(f.primary_columns,to_jsonb(NEW)); new_time:=COALESCE((to_jsonb(NEW)->>f.clock_column)::timestamptz,'-infinity'::timestamptz); END IF;
 IF TG_WHEN='AFTER' THEN
  IF TG_OP='DELETE' THEN
   EXECUTE format('DELETE FROM %s WHERE af_identity=$1 AND af_time=$2 AND af_moving_xid IS NULL',f.registry_oid::regclass) USING old_key,old_time;
  ELSE
   -- The real payload INSERT/UPDATE has now passed original RLS WITH CHECK,
   -- NOT NULL and CHECK constraints. Only now may global key/scope change.
   IF TG_OP='UPDATE' THEN
    EXECUTE format('SELECT af_identity FROM %s WHERE af_identity=$1 AND af_moving_xid=$2 AND af_move_identity=$3 FOR UPDATE',f.registry_oid::regclass) INTO move_from USING old_key,txid_current(),new_key;
   ELSE
    EXECUTE format('SELECT af_identity FROM %s WHERE af_moving_xid=$1 AND af_move_identity=$2 FOR UPDATE',f.registry_oid::regclass) INTO move_from USING txid_current(),new_key;
   END IF;
   IF move_from IS NULL THEN
    IF TG_OP='UPDATE' THEN RAISE EXCEPTION 'pending global UPDATE marker missing'; END IF;
    PERFORM partition_ops.claim_row(f.family,to_jsonb(NEW),new_key);
   ELSE
    SELECT relation_oid::text INTO physical FROM partition_ops.partitions WHERE family=f.family AND (lower_at IS NULL OR new_time>=lower_at) AND new_time<upper_at AND state='active' FOR KEY SHARE;
    IF physical IS NULL THEN RAISE EXCEPTION 'moved row has no active partition'; END IF;
    patch:=partition_ops.projection(f.key_columns,to_jsonb(NEW))||jsonb_build_object('af_identity',new_key,'af_time',new_time,'af_partition',physical,'af_moving_xid',NULL,'af_move_identity',NULL);
    SELECT string_agg(format('%1$I=(jsonb_populate_record(NULL::%2$s,$1)).%1$I',a.attname,f.registry_oid::regclass),',') INTO assignments FROM pg_attribute a WHERE a.attrelid=f.registry_oid AND a.attnum>0 AND NOT a.attisdropped;
    EXECUTE format('UPDATE %s SET %s WHERE af_identity=$2 AND af_moving_xid=$3 AND af_move_identity=$4 RETURNING true',f.registry_oid::regclass,assignments) INTO exists_key USING patch,move_from,txid_current(),new_key;
    IF exists_key IS NOT TRUE THEN RAISE EXCEPTION 'global move marker changed'; END IF;
   END IF;
  END IF;
  RETURN NULL;
 END IF;
 IF TG_OP='DELETE' THEN
  PERFORM 1 FROM partition_ops.partitions WHERE family=f.family AND relation_oid=TG_RELID AND state='active' FOR KEY SHARE;
  IF NOT FOUND THEN RAISE EXCEPTION 'partition is sealed for retention'; END IF;
  RETURN OLD;
 ELSIF TG_OP='INSERT' THEN
  -- Hold the retention barrier before physical insertion; defer uniqueness to
  -- the first AFTER ROW trigger so original INSERT checks always run first.
  PERFORM 1 FROM partition_ops.partitions WHERE family=f.family AND relation_oid=TG_RELID AND state='active' FOR KEY SHARE;
  IF NOT FOUND THEN RAISE EXCEPTION 'partition is sealed for retention'; END IF;
 ELSIF TG_OP='UPDATE' THEN
  -- Serialize state changes with the retention barrier, including updates that
  -- only change a lease/call status and would otherwise resurrect old work.
  PERFORM 1 FROM partition_ops.partitions WHERE family=f.family AND relation_oid=TG_RELID AND state='active' FOR KEY SHARE;
  IF NOT FOUND THEN RAISE EXCEPTION 'partition is sealed for retention'; END IF;
  EXECUTE format('SELECT true FROM %s WHERE af_identity=$1 FOR UPDATE',f.registry_oid::regclass) INTO exists_key USING old_key;
  IF exists_key IS NOT TRUE THEN PERFORM partition_ops.claim_row(f.family,to_jsonb(OLD),old_key); END IF;
  SELECT relation_oid::text INTO physical FROM partition_ops.partitions WHERE family=f.family AND (lower_at IS NULL OR new_time>=lower_at) AND new_time<upper_at AND state='active' FOR KEY SHARE;
  IF physical IS NULL THEN RAISE EXCEPTION 'updated time has no active partition'; END IF;
  -- Mark row movement without changing original keys or scope before RLS.
  -- The partial index keeps AFTER INSERT lookup bounded to pending moves.
  EXECUTE format('UPDATE %s SET af_moving_xid=txid_current(),af_move_identity=$1 WHERE af_identity=$2',f.registry_oid::regclass) USING new_key,old_key;
 END IF;
 RETURN NEW;
END $$;
CREATE FUNCTION partition_ops.expand(p_family text,p_boundary timestamptz,p_admission_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp SET TimeZone='UTC' AS $$
DECLARE f partition_ops.families; source regclass; parent_name text; registry_name text; idx record; expr text; keys name[]; idxmap jsonb:='{}'; n integer:=0; col name; fk record; acl record; clock_expression text;
BEGIN
 PERFORM partition_ops.assert_owner();
 PERFORM pg_advisory_xact_lock(hashtextextended('agentforge.append.family:'||p_family,0));
 IF COALESCE(p_admission_sha256,'') !~ '^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'authority admission digest required'; END IF;
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.phase<>'registered' THEN
  IF f.boundary=p_boundary AND f.admission_sha256=p_admission_sha256 THEN RETURN to_jsonb(f); END IF;
  RAISE EXCEPTION 'resume scope differs from admitted expansion';
 END IF;
 PERFORM partition_ops.require_operator_approval(p_family,'expand',f.revision,p_admission_sha256);
 IF NOT EXISTS(SELECT 1 FROM partition_ops.operator_approvals WHERE family=p_family AND stage='expand' AND revision=f.revision AND (evidence->>'operation_boundary')::timestamptz IS NOT DISTINCT FROM p_boundary) THEN RAISE EXCEPTION 'operator approved boundary differs'; END IF;
 IF p_boundary IS NULL OR p_boundary<=clock_timestamp() OR p_boundary<>date_trunc(f.cadence,p_boundary,'UTC') THEN RAISE EXCEPTION 'UTC partition boundary required'; END IF;
 source:=p_family::regclass;
 IF NOT EXISTS(SELECT 1 FROM pg_class WHERE oid=source AND relkind='r' AND relowner=(SELECT oid FROM pg_roles WHERE rolname=current_user)) THEN RAISE EXCEPTION 'authority-owned ordinary source required'; END IF;
 IF EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid=source AND NOT tgisinternal AND tgname>='zzzz_af_global_before') THEN RAISE EXCEPTION 'source trigger ordering needs an explicit successor'; END IF;
 IF EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid=source AND NOT tgisinternal AND (tgtype & 2)=0 AND tgname<='AAAA_af_global_after') THEN RAISE EXCEPTION 'source AFTER trigger ordering needs an explicit successor'; END IF;
 -- JSON conflict candidates must equal original EXCLUDED after BEFORE INSERT.
 -- The frozen six upsert families have no candidate-mutating INSERT trigger;
 -- the existing checkpoint receipt trigger is validation-only. Any evolution
 -- needs a separately reviewed projection instead of silently changing merges.
 IF p_family IN ('gateway.usage_records','gateway.billing_events','gateway.price_book_v2_accounting_checkpoints','gateway.agent_traces','gateway.agent_trace_spans','gateway.agent_trace_events') AND EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid=source AND NOT tgisinternal AND (tgtype & 2)=2 AND (tgtype & 4)=4 AND tgname<>'price_book_v2_checkpoint_bound') THEN RAISE EXCEPTION 'upsert BEFORE INSERT projection requires explicit successor'; END IF;
 IF EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid=source AND attname LIKE 'af_%' AND attnum>0 AND NOT attisdropped) THEN RAISE EXCEPTION 'reserved directory column collision'; END IF;
 IF NOT EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid=source AND attname=f.clock_column AND atttypid='timestamptz'::regtype) THEN RAISE EXCEPTION 'expected timestamptz clock absent'; END IF;
 SELECT array_agg(a.attname ORDER BY k.ordinality) INTO keys FROM pg_index i CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum,ordinality) JOIN pg_attribute a ON a.attrelid=source AND a.attnum=k.attnum WHERE i.indrelid=source AND i.indisprimary;
 IF keys IS NULL THEN RAISE EXCEPTION 'global primary identity required'; END IF;
 -- Capture every existing unique-index dependency, including expression/partial
 -- keys and NULLS NOT DISTINCT. No global request index is dropped or weakened.
 SELECT array_agg(DISTINCT a.attname ORDER BY a.attname) INTO f.key_columns FROM pg_attribute a WHERE a.attrelid=source AND a.attnum>0 AND NOT a.attisdropped AND (
 a.attname IN('tenant_id','user_id','session_id') OR EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid=source AND i.indisunique AND a.attnum=ANY(i.indkey)) OR EXISTS(SELECT 1 FROM pg_index i JOIN pg_depend d ON d.classid='pg_class'::regclass AND d.objid=i.indexrelid WHERE i.indrelid=source AND i.indisunique AND d.refobjid=source AND d.refobjsubid=a.attnum));
 IF EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid=source AND attname=ANY(f.key_columns) AND attgenerated<>'') THEN RAISE EXCEPTION 'generated unique-key evolution needs an explicit successor'; END IF;
 parent_name:=format('%I.%I',split_part(p_family,'.',1),split_part(p_family,'.',2)||'_af_next');
 registry_name:=format('partition_ops.%I',replace(p_family,'.','_')||'_keys');
 EXECUTE format('CREATE TABLE %s AS SELECT * FROM %s WITH NO DATA',registry_name,source);
 EXECUTE format('ALTER TABLE %s ADD COLUMN af_identity jsonb PRIMARY KEY, ADD COLUMN af_time timestamptz NOT NULL, ADD COLUMN af_partition text NOT NULL, ADD COLUMN af_moving_xid bigint, ADD COLUMN af_move_identity jsonb',registry_name);
 EXECUTE format('CREATE INDEX %I ON %s(af_moving_xid,af_move_identity) WHERE af_moving_xid IS NOT NULL','af_moving_'||source::oid,registry_name);
 FOR idx IN SELECT i.*,pg_get_indexdef(i.indexrelid) def,c.relname FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid WHERE i.indrelid=source AND i.indisunique ORDER BY i.indexrelid LOOP
  IF NOT idx.indisvalid OR NOT idx.indisready THEN RAISE EXCEPTION 'invalid global source index'; END IF;
  n:=n+1; expr:=substring(idx.def FROM ' USING .*$');
  EXECUTE format('CREATE UNIQUE INDEX %I ON %s %s','af_global_'||source::oid||'_'||n,registry_name,expr);
  idxmap:=idxmap||jsonb_build_object(idx.relname,jsonb_build_object('oid',idx.indexrelid,'registry_name','af_global_'||source::oid||'_'||n,'definition',idx.def,'predicate',pg_get_expr(idx.indpred,source),'expressions',(SELECT jsonb_agg(pg_get_indexdef(idx.indexrelid,k,true) ORDER BY k) FROM generate_series(1,idx.indnkeyatts) k),'nulls_not_distinct',idx.indnullsnotdistinct));
 END LOOP;
 -- Native NOT NULL clocks keep their ordinary range key: existing bounded
 -- queries prune without changing business predicates. Only legacy nullable
 -- clocks require expression routing and the adapter's additional predicates.
 SELECT CASE WHEN attnotnull THEN quote_ident(f.clock_column) ELSE format('(COALESCE(%I,''-infinity''::timestamptz))',f.clock_column) END INTO STRICT clock_expression FROM pg_attribute WHERE attrelid=source AND attname=f.clock_column AND attnum>0 AND NOT attisdropped;
 EXECUTE format('CREATE TABLE %s (LIKE %s INCLUDING DEFAULTS INCLUDING GENERATED INCLUDING CONSTRAINTS INCLUDING STORAGE INCLUDING COMMENTS) PARTITION BY RANGE (%s)',parent_name,source,clock_expression);
 -- Empty shadow index construction is transactional; the large legacy indexes
 -- remain attached to the old heap. No blocking index build on the old heap.
 FOR idx IN SELECT i.*,pg_get_indexdef(i.indexrelid) def FROM pg_index i WHERE i.indrelid=source ORDER BY i.indexrelid LOOP
  n:=n+1; expr:=replace(substring(idx.def FROM ' USING .*$'),' NULLS NOT DISTINCT','');
  EXECUTE format('CREATE INDEX %I ON %s %s','af_payload_'||source::oid||'_'||n,parent_name,expr);
 END LOOP;
 EXECUTE format('CREATE INDEX %I ON %s USING brin (%I)','af_brin_'||source::oid,parent_name,f.clock_column);
 FOR fk IN SELECT pg_get_constraintdef(oid) def,conname FROM pg_constraint WHERE conrelid=source AND contype='f' AND conparentid=0 LOOP
  EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s',parent_name,fk.conname,fk.def);
 END LOOP;
 FOR acl IN SELECT c.oid, x.grantee FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) x WHERE c.oid IN(registry_name::regclass,parent_name::regclass) AND x.grantee<>c.relowner LOOP
  EXECUTE format('REVOKE ALL ON %s FROM %s',acl.oid::regclass,CASE WHEN acl.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(acl.grantee)) END);
 END LOOP;
 UPDATE partition_ops.families SET source_oid=source,parent_oid=parent_name::regclass,registry_oid=registry_name::regclass,primary_columns=keys,key_columns=f.key_columns,indexes=idxmap,boundary=p_boundary,admission_sha256=p_admission_sha256,phase='expanded',revision=revision+1 WHERE family=p_family;
 INSERT INTO partition_ops.partitions(family,relation_oid,relation_name,lower_at,upper_at,legacy,state) VALUES(p_family,source,source::text,NULL,p_boundary,true,'active');
 EXECUTE format('CREATE TRIGGER zzzz_af_global_before BEFORE INSERT OR UPDATE OR DELETE ON %s FOR EACH ROW EXECUTE FUNCTION partition_ops.row_guard(%L)',source,p_family);
 EXECUTE format('CREATE TRIGGER "AAAA_af_global_after" AFTER INSERT OR UPDATE OR DELETE ON %s FOR EACH ROW EXECUTE FUNCTION partition_ops.row_guard(%L)',source,p_family);
 -- Global directory FK is validated online after bounded backfill; it is
 -- deferred so key/time UPDATE can preserve original FK update actions.
 SELECT string_agg(format('%I',k),',') INTO expr FROM unnest(keys) k;
 EXECUTE format('ALTER TABLE %s ADD CONSTRAINT af_directory_identity FOREIGN KEY(%s) REFERENCES %s(%s) DEFERRABLE INITIALLY DEFERRED NOT VALID',source,expr,registry_name,expr);
 EXECUTE format('ALTER TABLE %s ADD CONSTRAINT af_directory_identity FOREIGN KEY(%s) REFERENCES %s(%s) DEFERRABLE INITIALLY DEFERRED',parent_name,expr,registry_name,expr);
 EXECUTE format('ALTER TABLE %s ADD CONSTRAINT af_legacy_bound CHECK (%s<%L::timestamptz) NOT VALID',source,clock_expression,p_boundary);
 RETURN partition_ops.status(p_family);
END $$;

CREATE TABLE partition_ops.operator_approvals (
 family text NOT NULL REFERENCES partition_ops.families(family), stage text NOT NULL CHECK(stage IN('expand','cutover')),
 revision bigint NOT NULL CHECK(revision>=0), scope_sha256 text NOT NULL CHECK(scope_sha256~'^[0-9a-f]{64}$'),
 evidence jsonb NOT NULL CHECK(jsonb_typeof(evidence)='object'), approved_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(family,stage,revision)
);
CREATE FUNCTION partition_ops.owner_family_enabled(p_family text) RETURNS boolean LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
DECLARE result boolean;
BEGIN
 PERFORM partition_ops.assert_owner();
 PERFORM pg_advisory_xact_lock_shared(hashtextextended('agentforge.append.family:'||p_family,0));
 SELECT phase<>'registered' INTO STRICT result FROM partition_ops.families WHERE family=p_family;
 RETURN result;
END $$;
CREATE FUNCTION partition_ops.approve_operator_stage(p_family text,p_stage text,p_revision bigint,p_scope_sha256 text,p_evidence jsonb)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; a partition_ops.operator_approvals; field text;
BEGIN
 PERFORM partition_ops.assert_owner();
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF p_stage IS NULL OR p_stage NOT IN('expand','cutover') OR p_revision IS DISTINCT FROM f.revision OR COALESCE(p_scope_sha256,'') !~ '^[0-9a-f]{64}$'
 OR (p_stage='expand' AND f.phase<>'registered') OR (p_stage='cutover' AND f.phase<>'validated')
 OR p_evidence IS NULL OR jsonb_typeof(p_evidence)<>'object' OR p_evidence->>'family' IS DISTINCT FROM p_family
 OR p_evidence->>'stage' IS DISTINCT FROM p_stage OR p_evidence->>'operation_scope_sha256' IS DISTINCT FROM p_scope_sha256
 OR (p_stage='expand' AND (NULLIF(p_evidence->>'operation_boundary','') IS NULL OR (p_evidence->>'operation_boundary')::timestamptz<=clock_timestamp()))
 THEN RAISE EXCEPTION 'exact operator phase/revision/scope evidence required'; END IF;
 FOR field IN SELECT unnest(ARRAY['authority_snapshot_sha256','pitr_restore_drill_sha256','role_rls_acceptance_sha256','n_minus_one_compatibility_sha256','dependency_rebind_sha256','authority_approval_sha256','native_writer_acceptance_sha256']) LOOP
  IF COALESCE(p_evidence->>field,'') !~ '^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'real operator artifact digests required: %',field; END IF;
 END LOOP;
 SELECT * INTO a FROM partition_ops.operator_approvals WHERE family=p_family AND stage=p_stage AND revision=p_revision;
 IF FOUND THEN
  IF a.scope_sha256 IS DISTINCT FROM p_scope_sha256 OR a.evidence IS DISTINCT FROM p_evidence THEN RAISE EXCEPTION 'operator approval replacement refused'; END IF;
  RETURN to_jsonb(a);
 END IF;
 INSERT INTO partition_ops.operator_approvals(family,stage,revision,scope_sha256,evidence) VALUES(p_family,p_stage,p_revision,p_scope_sha256,p_evidence) RETURNING * INTO a;
 RETURN to_jsonb(a);
END $$;
CREATE FUNCTION partition_ops.require_operator_approval(p_family text,p_stage text,p_revision bigint,p_scope_sha256 text)
RETURNS void LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 PERFORM partition_ops.assert_owner();
 PERFORM 1 FROM partition_ops.operator_approvals WHERE family=p_family AND stage=p_stage AND revision=p_revision AND scope_sha256=p_scope_sha256 FOR KEY SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'separate operator authority approval required before expand/cutover'; END IF;
END $$;
-- Independent authority functions. Every call is one bounded, resumable owner
-- transaction. The native authority must load expand.sql before this proposal.
-- Native runner enforces the original2s lock/60s statement budget.
CREATE FUNCTION partition_ops.status(p_family text) RETURNS jsonb LANGUAGE sql
STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT to_jsonb(f)||jsonb_build_object('partitions',COALESCE((SELECT jsonb_agg(to_jsonb(p)||jsonb_build_object('attached',EXISTS(SELECT 1 FROM pg_inherits i WHERE i.inhrelid=p.relation_oid AND i.inhparent=f.parent_oid),'detach_pending',COALESCE((SELECT i.inhdetachpending FROM pg_inherits i WHERE i.inhrelid=p.relation_oid AND i.inhparent=f.parent_oid),false)) ORDER BY p.upper_at) FROM partition_ops.partitions p WHERE p.family=f.family),'[]'::jsonb),'foreign_keys',COALESCE((SELECT jsonb_agg(to_jsonb(k) ORDER BY k.original_oid) FROM partition_ops.foreign_keys k WHERE k.target_family=f.family),'[]'::jsonb)) FROM partition_ops.families f WHERE f.family=p_family
$$;
CREATE FUNCTION partition_ops.backfill(p_family text,p_expected_revision bigint,p_limit integer DEFAULT 10000)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; r record; pk text; pk_anchor text; where_after text; row_key jsonb; exists_row jsonb; copied integer:=0;
BEGIN
 PERFORM partition_ops.assert_owner();
 IF p_limit<1 OR p_limit>10000 THEN RAISE EXCEPTION 'bounded backfill required'; END IF;
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.revision IS DISTINCT FROM p_expected_revision THEN RAISE EXCEPTION 'stale migration revision'; END IF;
 IF f.phase='backfilled' THEN RETURN partition_ops.status(p_family); END IF;
 IF f.phase<>'expanded' THEN RAISE EXCEPTION 'expansion required'; END IF;
 SELECT string_agg(format('%I',c),',') INTO pk FROM unnest(f.primary_columns) c;
 where_after:=CASE WHEN f.cursor IS NULL THEN 'true' ELSE format('ROW(%s)>(SELECT ROW(%s) FROM jsonb_populate_record(NULL::%s,$1))',pk,pk,f.source_oid::regclass) END;
 -- Keyset order uses the actual typed source PK and collation, never lexical
 -- JSON ordering or OFFSET. FOR SHARE prevents a stale directory backfill.
 FOR r IN EXECUTE format('SELECT to_jsonb(t) body FROM %s t WHERE %s ORDER BY %s LIMIT %s FOR SHARE',f.source_oid::regclass,where_after,pk,p_limit) USING f.cursor LOOP
  row_key:=partition_ops.identity(f.primary_columns,r.body);
  EXECUTE format('SELECT to_jsonb(k) FROM %s k WHERE af_identity=$1 FOR UPDATE',f.registry_oid::regclass) INTO exists_row USING row_key;
  IF exists_row IS NULL THEN PERFORM partition_ops.claim_row(f.family,r.body,row_key);
  ELSIF partition_ops.projection(f.key_columns,exists_row)<>partition_ops.projection(f.key_columns,r.body)
    OR (exists_row->>'af_time')::timestamptz<>COALESCE((r.body->>f.clock_column)::timestamptz,'-infinity'::timestamptz)
  THEN RAISE EXCEPTION 'directory/source identity mismatch'; END IF;
  f.cursor:=row_key;copied:=copied+1;
 END LOOP;
 UPDATE partition_ops.families SET cursor=f.cursor,phase=CASE WHEN copied=0 THEN 'backfilled' ELSE 'expanded' END,revision=revision+1 WHERE family=p_family;
 RETURN partition_ops.status(p_family)||jsonb_build_object('batch_rows',copied);
END $$;
CREATE FUNCTION partition_ops.validate_legacy(p_family text,p_expected_revision bigint)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families;
BEGIN
 PERFORM partition_ops.assert_owner();
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.revision IS DISTINCT FROM p_expected_revision OR f.phase NOT IN('backfilled','validated') THEN RAISE EXCEPTION 'stale or incomplete backfill'; END IF;
 -- Separate from cutover; CHECK and FK validation scans never occur under the
 -- cutover ACCESS EXCLUSIVE lock. NOT VALID already checks concurrent writes.
 EXECUTE format('ALTER TABLE %s VALIDATE CONSTRAINT af_directory_identity',f.source_oid::regclass);
 EXECUTE format('ALTER TABLE %s VALIDATE CONSTRAINT af_legacy_bound',f.source_oid::regclass);
 UPDATE partition_ops.families SET phase='validated',revision=revision+1 WHERE family=p_family;
 RETURN partition_ops.status(p_family);
END $$;
CREATE FUNCTION partition_ops.capture_foreign_keys(p_family text) RETURNS jsonb
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; c record; src name[]; dst name[];
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.phase NOT IN('expanded','backfilled','validated') THEN RAISE EXCEPTION 'expanded family required'; END IF;
 FOR c IN SELECT * FROM pg_constraint WHERE contype='f' AND confrelid=f.source_oid AND conparentid=0 AND conname<>'af_directory_identity' ORDER BY oid LOOP
  SELECT array_agg(a.attname ORDER BY k.ord) INTO src FROM unnest(c.conkey) WITH ORDINALITY k(n,ord) JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.n;
  SELECT array_agg(a.attname ORDER BY k.ord) INTO dst FROM unnest(c.confkey) WITH ORDINALITY k(n,ord) JOIN pg_attribute a ON a.attrelid=c.confrelid AND a.attnum=k.n;
  INSERT INTO partition_ops.foreign_keys(original_oid,source_oid,target_family,original_name,original_definition,replacement_name,source_columns,target_columns)
  VALUES(c.oid,c.conrelid,p_family,c.conname,pg_get_constraintdef(c.oid),'af_fk_'||c.oid,src,dst)
  ON CONFLICT(original_oid) DO NOTHING;
 END LOOP;
 RETURN partition_ops.status(p_family);
END $$;
-- All referencing tables (including nonpartitioned eval scores/examples and
-- composite lease scopes) retain the original FK columns, MATCH, actions and
-- deferrability. They point to the typed global directory, never per-month IDs.
CREATE FUNCTION partition_ops.reference_guard() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE k partition_ops.foreign_keys; f partition_ops.families; clauses text; target_partition text; row_json jsonb:=to_jsonb(NEW); n integer;
BEGIN
 SELECT * INTO STRICT k FROM partition_ops.foreign_keys WHERE original_oid=TG_ARGV[0]::oid;
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=k.target_family;
 IF TG_NARGS<>1 OR TG_LEVEL<>'ROW' OR NOT (TG_RELID=k.source_oid OR EXISTS(SELECT 1 FROM pg_partition_ancestors(TG_RELID) a WHERE a.relid=k.source_oid)) THEN RAISE EXCEPTION 'invalid reference guard'; END IF;
 -- MATCH SIMPLE/FULL NULL behaviour remains the original actual FK's job.
 IF EXISTS(SELECT 1 FROM unnest(k.source_columns) col WHERE row_json->col='null'::jsonb OR NOT row_json ? col) THEN RETURN NEW; END IF;
 clauses:=''; FOR n IN 1..cardinality(k.source_columns) LOOP
  clauses:=clauses||CASE WHEN n>1 THEN ' AND ' ELSE '' END||format('%I=(jsonb_populate_record(NULL::%s,$1)).%I',k.target_columns[n],k.source_oid::regclass,k.source_columns[n]);
 END LOOP;
 EXECUTE format('SELECT af_partition FROM %s WHERE %s',f.registry_oid::regclass,clauses) INTO target_partition USING row_json;
 IF target_partition IS NULL THEN RETURN NEW; END IF; -- actual FK rejects absence
 PERFORM 1 FROM partition_ops.partitions WHERE family=f.family AND relation_oid::text=target_partition AND state='active' FOR KEY SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'reference targets a sealed or retired partition'; END IF;
 RETURN NEW;
END $$;
CREATE FUNCTION partition_ops.rebind_fk(p_oid oid,p_step text) RETURNS jsonb LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
DECLARE k partition_ops.foreign_keys; f partition_ops.families; definition text; regular boolean;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT k FROM partition_ops.foreign_keys WHERE original_oid=p_oid FOR UPDATE;
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=k.target_family;
 IF p_step='expand' AND k.phase='captured' THEN
  definition:=replace(k.original_definition,'REFERENCES '||f.source_oid::regclass::text||'(', 'REFERENCES '||f.registry_oid::regclass::text||'(');
  IF definition=k.original_definition THEN RAISE EXCEPTION 'FK definition replacement did not bind original target'; END IF;
  SELECT relkind='r' INTO regular FROM pg_class WHERE oid=k.source_oid;
  -- PostgreSQL cannot add a NOT VALID FK to a partitioned referencing parent.
  -- That rare scan is an explicit, bounded standalone authority phase, never
  -- hidden inside cutover. Ordinary legacy heaps use expand/validate/contract.
  EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s%s',k.source_oid::regclass,k.replacement_name,definition,CASE WHEN regular THEN ' NOT VALID' ELSE '' END);
  EXECUTE format('CREATE TRIGGER %I BEFORE INSERT OR UPDATE ON %s FOR EACH ROW EXECUTE FUNCTION partition_ops.reference_guard(%L)','af_live_ref_'||p_oid,k.source_oid::regclass,p_oid::text);
  UPDATE partition_ops.foreign_keys SET phase='expanded' WHERE original_oid=p_oid;
 ELSIF p_step='validate' AND k.phase='expanded' THEN
  EXECUTE format('ALTER TABLE %s VALIDATE CONSTRAINT %I',k.source_oid::regclass,k.replacement_name);
  UPDATE partition_ops.foreign_keys SET phase='validated' WHERE original_oid=p_oid;
 ELSIF p_step='contract' AND k.phase='validated' THEN
  IF NOT EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid=k.source_oid AND conname=k.replacement_name AND convalidated) THEN RAISE EXCEPTION 'validated replacement required'; END IF;
  EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I',k.source_oid::regclass,k.original_name);
  EXECUTE format('ALTER TABLE %s RENAME CONSTRAINT %I TO %I',k.source_oid::regclass,k.replacement_name,k.original_name);
  UPDATE partition_ops.foreign_keys SET phase='rebound' WHERE original_oid=p_oid;
 ELSIF NOT ((p_step='expand' AND k.phase IN('expanded','validated','rebound')) OR (p_step='validate' AND k.phase IN('validated','rebound')) OR (p_step='contract' AND k.phase='rebound')) THEN RAISE EXCEPTION 'FK phase transition refused'; END IF;
 RETURN (SELECT to_jsonb(x) FROM partition_ops.foreign_keys x WHERE original_oid=p_oid);
END $$;
CREATE FUNCTION partition_ops.prepare(p_family text,p_first timestamptz,p_count integer) RETURNS jsonb
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp SET TimeZone='UTC' AS $$
DECLARE f partition_ops.families; n integer; lo timestamptz; hi timestamptz; rel text; step interval; oid_value oid; acl record;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF p_first IS NULL OR p_count IS NULL OR f.phase='registered' OR p_count<1 OR p_count>CASE WHEN f.cadence='day' THEN 90 ELSE 24 END OR p_first<>date_trunc(f.cadence,p_first,'UTC') OR p_first<f.boundary THEN RAISE EXCEPTION 'bounded admitted UTC preparation required'; END IF;
 step:=CASE WHEN f.cadence='day' THEN interval '1 day' ELSE interval '1 month' END;
 FOR n IN 0..p_count-1 LOOP
  lo:=p_first+n*step;hi:=lo+step;
  rel:=format('%I.%I',split_part(p_family,'.',1),split_part(p_family,'.',2)||'_p'||to_char(lo AT TIME ZONE 'UTC',CASE WHEN f.cadence='day' THEN 'YYYYMMDD' ELSE 'YYYYMM' END));
  SELECT relation_oid INTO oid_value FROM partition_ops.partitions WHERE family=p_family AND relation_name=rel AND lower_at=lo AND upper_at=hi AND state='active';
  IF oid_value IS NOT NULL THEN
   IF to_regclass(rel) IS DISTINCT FROM oid_value OR NOT EXISTS(SELECT 1 FROM pg_inherits WHERE inhrelid=oid_value AND inhparent=f.parent_oid AND NOT inhdetachpending) THEN RAISE EXCEPTION 'existing partition identity changed'; END IF;
  ELSE
   IF to_regclass(rel) IS NOT NULL THEN RAISE EXCEPTION 'unregistered object collision'; END IF;
   EXECUTE format('CREATE TABLE %s PARTITION OF %s FOR VALUES FROM (%L) TO (%L)',rel,f.parent_oid::regclass,lo,hi);
   FOR acl IN SELECT x.grantee FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) x WHERE c.oid=rel::regclass AND x.grantee<>c.relowner LOOP
    EXECUTE format('REVOKE ALL ON %s FROM %s',rel,CASE WHEN acl.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(acl.grantee)) END);
   END LOOP;
   INSERT INTO partition_ops.partitions(family,relation_oid,relation_name,lower_at,upper_at,state) VALUES(p_family,rel::regclass,rel,lo,hi,'active');
  END IF;
 END LOOP;
 RETURN partition_ops.status(p_family);
END $$;
CREATE FUNCTION partition_ops.cutover(p_family text,p_revision bigint,p_compatibility_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; legacy_name text; c record; tr record; q text; acl record; policy record; seq record;
BEGIN
 PERFORM partition_ops.assert_owner();
 PERFORM pg_advisory_xact_lock(hashtextextended('agentforge.append.family:'||p_family,0)); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.phase='active' AND f.compatibility_sha256=p_compatibility_sha256 THEN RETURN partition_ops.status(p_family); END IF;
 PERFORM partition_ops.require_operator_approval(p_family,'cutover',p_revision,p_compatibility_sha256);
 IF f.phase<>'validated' OR f.revision IS DISTINCT FROM p_revision OR COALESCE(p_compatibility_sha256,'') !~ '^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'validated compatibility/revision gate required'; END IF;
 IF EXISTS(SELECT 1 FROM pg_constraint WHERE contype='f' AND confrelid=f.source_oid AND conparentid=0) THEN RAISE EXCEPTION 'all incoming FK expand/validate/rebind phases must complete'; END IF;
 IF EXISTS(SELECT 1 FROM partition_ops.foreign_keys WHERE target_family=p_family AND phase<>'rebound') THEN RAISE EXCEPTION 'unfinished FK replacement'; END IF;
 IF (SELECT count(*) FROM pg_constraint WHERE conrelid=f.source_oid AND conname IN('af_directory_identity','af_legacy_bound') AND convalidated)<>2 THEN RAISE EXCEPTION 'legacy constraints not validated'; END IF;
 IF NOT EXISTS(SELECT 1 FROM partition_ops.partitions WHERE family=p_family AND lower_at=f.boundary AND state='active') THEN RAISE EXCEPTION 'first future partition required'; END IF;
 IF jsonb_array_length(partition_ops.legacy_index_sql(p_family))<>0 THEN RAISE EXCEPTION 'valid matching CONCURRENTLY built legacy indexes required before cutover'; END IF;
 EXECUTE format('LOCK TABLE %s IN ACCESS EXCLUSIVE MODE',f.source_oid::regclass);
 -- Preserve original application triggers (including immutable receipts) and
 -- RLS, then remove leaf copies before parent ATTACH clones their exact names.
 FOR tr IN SELECT oid,tgname,pg_get_triggerdef(oid) def FROM pg_trigger WHERE tgrelid=f.source_oid AND NOT tgisinternal LOOP
  q:=replace(tr.def,' ON '||f.source_oid::regclass::text||' ',' ON '||f.parent_oid::regclass::text||' ');
  IF q=tr.def THEN RAISE EXCEPTION 'source trigger dependency must be reviewed'; END IF;
  EXECUTE q; EXECUTE format('DROP TRIGGER %I ON %s',tr.tgname,f.source_oid::regclass);
 END LOOP;
 FOR policy IN SELECT * FROM pg_policy WHERE polrelid=f.source_oid LOOP
  SELECT string_agg(CASE WHEN r=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(r)) END,',') INTO q FROM unnest(policy.polroles) r;
  EXECUTE format('CREATE POLICY %I ON %s AS %s FOR %s TO %s%s%s',policy.polname,f.parent_oid::regclass,CASE WHEN policy.polpermissive THEN 'PERMISSIVE' ELSE 'RESTRICTIVE' END,CASE policy.polcmd WHEN 'r' THEN 'SELECT' WHEN 'a' THEN 'INSERT' WHEN 'w' THEN 'UPDATE' WHEN 'd' THEN 'DELETE' ELSE 'ALL' END,q,CASE WHEN policy.polqual IS NULL THEN '' ELSE ' USING ('||pg_get_expr(policy.polqual,f.source_oid)||')' END,CASE WHEN policy.polwithcheck IS NULL THEN '' ELSE ' WITH CHECK ('||pg_get_expr(policy.polwithcheck,f.source_oid)||')' END);
 END LOOP;
 IF (SELECT relrowsecurity FROM pg_class WHERE oid=f.source_oid) THEN EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY',f.parent_oid::regclass); END IF;
 IF (SELECT relforcerowsecurity FROM pg_class WHERE oid=f.source_oid) THEN EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY',f.parent_oid::regclass); END IF;
 -- Exact table and column ACL, including grant options. No default app grant.
 FOR acl IN SELECT x.* FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) x WHERE c.oid=f.source_oid LOOP
  IF acl.grantee<>(SELECT relowner FROM pg_class WHERE oid=f.source_oid) THEN
   EXECUTE format('GRANT %s ON %s TO %s%s',acl.privilege_type,f.parent_oid::regclass,CASE WHEN acl.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(acl.grantee)) END,CASE WHEN acl.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
  END IF;
 END LOOP;
 FOR acl IN SELECT a.attname,x.* FROM pg_attribute a CROSS JOIN LATERAL aclexplode(a.attacl) x WHERE a.attrelid=f.source_oid AND a.attnum>0 LOOP
  EXECUTE format('GRANT %s(%I) ON %s TO %s%s',acl.privilege_type,acl.attname,f.parent_oid::regclass,CASE WHEN acl.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(acl.grantee)) END,CASE WHEN acl.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
 END LOOP;
 legacy_name:=split_part(p_family,'.',2)||'_af_legacy';
 EXECUTE format('ALTER TABLE %s RENAME TO %I',f.source_oid::regclass,legacy_name);
 EXECUTE format('ALTER TABLE %s RENAME TO %I',f.parent_oid::regclass,split_part(p_family,'.',2));
 EXECUTE format('ALTER TABLE %s ATTACH PARTITION %s FOR VALUES FROM (MINVALUE) TO (%L)',f.parent_oid::regclass,f.source_oid::regclass,f.boundary);
 UPDATE partition_ops.foreign_keys SET source_oid=f.parent_oid WHERE source_oid=f.source_oid;
 UPDATE partition_ops.partitions SET relation_name=f.source_oid::regclass::text WHERE relation_oid=f.source_oid;
 -- Directory rows store stable relation OIDs, so cutover does not mass-update
 -- keys. Transfer serial ownership before a future legacy partition DROP.
 FOR seq IN SELECT s.oid,a.attname FROM pg_depend d JOIN pg_class s ON s.oid=d.objid AND s.relkind='S' JOIN pg_attribute a ON a.attrelid=d.refobjid AND a.attnum=d.refobjsubid WHERE d.refobjid=f.source_oid AND d.classid='pg_class'::regclass AND d.deptype='a' LOOP
  EXECUTE format('ALTER SEQUENCE %s OWNED BY %s.%I',seq.oid::regclass,f.parent_oid::regclass,seq.attname);
 END LOOP;
 UPDATE partition_ops.families SET phase='active',compatibility_sha256=p_compatibility_sha256,revision=revision+1 WHERE family=p_family;
 RETURN partition_ops.status(p_family);
END $$;
CREATE FUNCTION partition_ops.legacy_index_sql(p_family text) RETURNS jsonb LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; i record; tail text; result jsonb:='[]'; index_name text;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 IF f.phase NOT IN('expanded','backfilled','validated') THEN RAISE EXCEPTION 'legacy index preparation requires expanded source'; END IF;
 FOR i IN SELECT * FROM pg_index WHERE indrelid=f.parent_oid LOOP
  tail:=substring(pg_get_indexdef(i.indexrelid) FROM ' USING .*$');
  IF NOT EXISTS(SELECT 1 FROM pg_index x WHERE x.indrelid=f.source_oid AND x.indisvalid AND x.indisready AND x.indisunique=i.indisunique AND substring(pg_get_indexdef(x.indexrelid) FROM ' USING .*$')=tail) THEN
   index_name:='af_legacy_'||f.source_oid||'_'||i.indexrelid;
   IF to_regclass(format('%I.%I',split_part(p_family,'.',1),index_name)) IS NOT NULL THEN RAISE EXCEPTION 'invalid or incompatible interrupted index: inspect and explicitly remove/rebuild through authority'; END IF;
   result:=result||jsonb_build_array(format('CREATE INDEX CONCURRENTLY %I ON %s %s',index_name,f.source_oid::regclass,tail));
  END IF;
 END LOOP;
 RETURN result;
END $$;

-- Caller-invoker compatibility entry: original INSERT/UPDATE/SELECT privileges
-- and FORCE RLS still apply to the real payload relation. Never grant an app
-- direct directory access or execute the invoker writer as a universal definer.
-- Native runner enforces the original2s lock/60s statement budget.
CREATE FUNCTION partition_ops.write_spec(p_family text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; relation oid;
BEGIN
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 relation:=CASE WHEN f.phase='active' THEN f.parent_oid ELSE f.source_oid END;
 IF relation IS NULL OR NOT has_table_privilege(session_user,relation,'INSERT') THEN RAISE EXCEPTION 'original table INSERT privilege required'; END IF;
 RETURN jsonb_build_object('relation',relation,'primary_columns',f.primary_columns,'phase',f.phase,'source_oid',f.source_oid,'parent_oid',f.parent_oid,'registry_oid',f.registry_oid);
END $$;
CREATE FUNCTION partition_ops.lookup_conflict(p_family text,p_index text,p_row jsonb) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; spec jsonb; expression text; clauses text; existing jsonb; sep text:=''; predicate text;
BEGIN
 PERFORM partition_ops.write_spec(p_family);
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 IF 'tenant_id'=ANY(f.key_columns) AND (NULLIF(current_setting('app.tenant_id',true),'') IS NULL OR p_row->>'tenant_id' IS DISTINCT FROM current_setting('app.tenant_id',true)) THEN RAISE EXCEPTION 'bound transaction tenant required for conflict lookup'; END IF;
 IF p_family IN ('gateway.agent_trace_spans','gateway.agent_trace_events') AND (NULLIF(current_setting('app.tenant_id',true),'') IS NULL OR NOT EXISTS(SELECT 1 FROM gateway.agent_traces t WHERE t.trace_id=(p_row->>'trace_id')::uuid AND t.tenant_id=current_setting('app.tenant_id',true))) THEN RAISE EXCEPTION 'bound trace tenant required'; END IF;
 spec:=f.indexes->p_index; IF spec IS NULL THEN RAISE EXCEPTION 'unknown original global conflict index'; END IF;
 predicate:=COALESCE(spec->>'predicate','true'); clauses:='('||predicate||') AND (SELECT '||predicate||' FROM jsonb_populate_record(NULL::'||f.registry_oid::regclass||',$1))';
 FOR expression IN SELECT jsonb_array_elements_text(spec->'expressions') LOOP
  clauses:=clauses||' AND ('||expression||')'||CASE WHEN (spec->>'nulls_not_distinct')::boolean THEN ' IS NOT DISTINCT FROM ' ELSE ' = ' END||'(SELECT '||expression||' FROM jsonb_populate_record(NULL::'||f.registry_oid::regclass||',$1))';
 END LOOP;
 EXECUTE format('SELECT to_jsonb(k) FROM %s k WHERE %s',f.registry_oid::regclass,clauses) INTO existing USING p_row;
 IF existing IS NULL AND f.phase<>'active' THEN
  EXECUTE format('SELECT to_jsonb(k)||jsonb_build_object(''af_identity'',partition_ops.identity($2,to_jsonb(k)),''af_time'',COALESCE(%I,''-infinity''::timestamptz),''af_partition'',tableoid::text) FROM %s k WHERE %s',f.clock_column,f.source_oid::regclass,clauses) INTO existing USING p_row,f.primary_columns;
 END IF;
 -- No cross-tenant/user identity data is returned by the definer. Global
 -- DO NOTHING collisions need no lookup and return only inserted=false.
 IF existing IS NOT NULL AND ((existing ? 'tenant_id') AND existing->'tenant_id' IS DISTINCT FROM p_row->'tenant_id' OR (existing ? 'user_id') AND existing->'user_id' IS DISTINCT FROM p_row->'user_id' OR (existing ? 'session_id') AND existing->'session_id' IS DISTINCT FROM p_row->'session_id' OR p_family IN ('gateway.agent_trace_spans','gateway.agent_trace_events') AND NOT EXISTS(SELECT 1 FROM gateway.agent_traces t WHERE t.trace_id=(existing->>'trace_id')::uuid AND t.tenant_id=current_setting('app.tenant_id',true))) THEN
  -- Preserve a native guarded upsert's empty RETURNING result while exposing
  -- neither a foreign identity nor its clock through the metadata definer.
  RETURN jsonb_build_object('scope_mismatch',true);
 END IF;
 RETURN CASE WHEN existing IS NULL THEN NULL ELSE jsonb_build_object('identity',existing->'af_identity','time',existing->'af_time','partition',existing->'af_partition','partition_state',(SELECT state FROM partition_ops.partitions WHERE relation_oid::text=existing->>'af_partition')) END;
END $$;
CREATE TABLE partition_ops.statement_policies(
 family text NOT NULL REFERENCES partition_ops.families(family), statement_tail text NOT NULL,
 target_columns name[], conflict_index name, action text NOT NULL CHECK(action IN('nothing','update')),
 update_sql text, where_sql text, PRIMARY KEY(family,statement_tail),
 CHECK((action='update')=(update_sql IS NOT NULL))
);
REVOKE ALL ON partition_ops.statement_policies FROM PUBLIC;
CREATE FUNCTION partition_ops.statement_spec(p_family text,p_tail text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE p partition_ops.statement_policies; f partition_ops.families; index_name text; item record;
BEGIN
 PERFORM partition_ops.write_spec(p_family);
 IF NOT partition_ops.family_enabled(p_family) THEN RAISE EXCEPTION 'family compatibility is not admitted'; END IF;
 SELECT * INTO STRICT p FROM partition_ops.statement_policies WHERE family=p_family AND statement_tail=p_tail;
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 index_name:=p.conflict_index;
 IF p.target_columns IS NOT NULL THEN
  FOR item IN SELECT key,value FROM jsonb_each(f.indexes) LOOP
   IF item.value->'expressions'=to_jsonb(p.target_columns) AND item.value->>'predicate' IS NULL THEN
    IF index_name IS NOT NULL THEN RAISE EXCEPTION 'ambiguous original conflict index'; END IF;
    index_name:=item.key;
   END IF;
  END LOOP;
  IF index_name IS NULL THEN RAISE EXCEPTION 'original conflict target absent'; END IF;
 END IF;
 IF p.action='update' AND index_name IS NULL THEN RAISE EXCEPTION 'update needs an exact global target'; END IF;
 RETURN to_jsonb(p)||jsonb_build_object('index',index_name);
END $$;
CREATE FUNCTION partition_ops.column_types(p_family text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE spec jsonb; result jsonb;
BEGIN
 spec:=partition_ops.write_spec(p_family);
 SELECT jsonb_object_agg(attname,format_type(atttypid,atttypmod)) INTO result FROM pg_attribute WHERE attrelid=(spec->>'relation')::oid AND attnum>0 AND NOT attisdropped AND attgenerated='';
 RETURN result;
END $$;
-- A replay whose payload has expired is never evidence that accounting is
-- absent. An update caller must re-observe/reconcile rather than charge again.
CREATE FUNCTION partition_ops.require_payload(p_result jsonb) RETURNS jsonb LANGUAGE plpgsql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF p_result->>'outcome'='retained_identity' THEN RAISE EXCEPTION 'retired append identity: reconciliation required' USING ERRCODE='23505'; END IF;
 RETURN p_result->'row';
END $$;
CREATE FUNCTION partition_ops.write_json(p_family text,p_row jsonb,p_mode text DEFAULT 'insert',p_conflict_index text DEFAULT NULL,p_statement text DEFAULT NULL,p_parameters jsonb DEFAULT '{}'::jsonb)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE spec jsonb; rel regclass; col record; columns text:=''; selections text:=''; sep text:=''; existing jsonb; found_row jsonb; inserted_row jsonb; condition text; key text; supplied text; assignments text; requested jsonb:=p_row; error_schema text; error_table text; error_relation oid; checked_identity jsonb; policy jsonb; alias_name text; qualified_condition text;
BEGIN
 IF p_row IS NULL OR p_mode IS NULL OR jsonb_typeof(p_row)<>'object' OR pg_column_size(p_row)>16777216 OR p_mode NOT IN('insert','ignore','same_or_error','usage_merge','statement') THEN RAISE EXCEPTION 'bounded known append write required'; END IF;
 spec:=partition_ops.write_spec(p_family); rel:=(spec->>'relation')::oid::regclass;
 IF p_mode='statement' THEN
  policy:=partition_ops.statement_spec(p_family,p_statement);p_conflict_index:=policy->>'index';
  IF p_parameters IS NULL OR jsonb_typeof(p_parameters)<>'object' OR EXISTS(SELECT 1 FROM jsonb_object_keys(p_parameters) k WHERE k<>'p31') OR (p_parameters<>'{}'::jsonb AND (p_family<>'gateway.agent_traces' OR jsonb_typeof(p_parameters->'p31') NOT IN ('string','null'))) THEN RAISE EXCEPTION 'unadmitted statement parameters'; END IF;
 END IF;
 FOR supplied IN SELECT jsonb_object_keys(p_row) LOOP
  IF NOT EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid=rel AND attname=supplied AND attnum>0 AND NOT attisdropped AND attgenerated='') THEN RAISE EXCEPTION 'unknown or generated write column'; END IF;
 END LOOP;
 FOR col IN SELECT a.attname,a.attgenerated,pg_get_expr(d.adbin,d.adrelid) default_expr FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid=rel AND a.attnum>0 AND NOT a.attisdropped AND a.attgenerated='' ORDER BY a.attnum LOOP
  columns:=columns||sep||quote_ident(col.attname);
  selections:=selections||sep||CASE WHEN p_row ? col.attname THEN format('(jsonb_populate_record(NULL::%s,$1)).%I',rel,col.attname) ELSE COALESCE(col.default_expr,'NULL') END||format(' AS %I',col.attname);
  sep:=',';
 END LOOP;
 -- Materialize the real typed defaults once, including generated UUIDs and
 -- default status/cost/metadata. A duplicate must not merge missing status as
 -- NULL when the original INSERT would have produced status='success'.
 EXECUTE format('SELECT to_jsonb(candidate) FROM (SELECT %s) candidate',selections) INTO p_row USING p_row;
 SELECT COALESCE(jsonb_object_agg(k,p_row->k),'{}'::jsonb) INTO requested FROM jsonb_object_keys(requested) k;
 SELECT string_agg(format('(jsonb_populate_record(NULL::%s,$1)).%I',rel,a.attname),',' ORDER BY a.attnum) INTO selections FROM pg_attribute a WHERE a.attrelid=rel AND a.attnum>0 AND NOT a.attisdropped AND a.attgenerated='';
 BEGIN
  -- This INSERT and every UPDATE/SELECT below run with the caller's original
  -- privileges and RLS. The directory trigger alone is SECURITY DEFINER.
  EXECUTE format('INSERT INTO %s(%s) SELECT %s RETURNING to_jsonb(%s.*)',rel,columns,selections,rel) INTO inserted_row USING p_row;
  RETURN jsonb_build_object('inserted',true,'outcome','inserted','row',inserted_row);
 EXCEPTION WHEN unique_violation THEN
  GET STACKED DIAGNOSTICS error_schema=SCHEMA_NAME,error_table=TABLE_NAME;
  IF NULLIF(error_schema,'') IS NULL OR NULLIF(error_table,'') IS NULL THEN RAISE; END IF;
  error_relation:=to_regclass(format('%I.%I',error_schema,error_table));
  IF error_relation IS NULL OR NOT (error_relation IN((spec->>'source_oid')::oid,(spec->>'parent_oid')::oid,(spec->>'registry_oid')::oid) OR EXISTS(SELECT 1 FROM pg_partition_ancestors(error_relation) a WHERE a.relid=(spec->>'parent_oid')::oid)) THEN RAISE; END IF;
  IF p_mode='insert' THEN RAISE; END IF;
  IF p_mode='ignore' OR (p_mode='statement' AND policy->>'action'='nothing' AND p_conflict_index IS NULL) THEN RETURN jsonb_build_object('inserted',false,'outcome','duplicate','row',NULL); END IF;
 END;
 IF p_conflict_index IS NULL THEN RAISE EXCEPTION 'original conflict index required'; END IF;
 existing:=partition_ops.lookup_conflict(p_family,p_conflict_index,p_row);
 IF existing IS NULL THEN RAISE EXCEPTION 'collision outside selected ON CONFLICT identity' USING ERRCODE='23505'; END IF;
 IF existing->>'scope_mismatch'='true' THEN
  IF p_mode='statement' THEN RETURN jsonb_build_object('inserted',false,'outcome','guard_conflict','row',NULL); END IF;
  RAISE EXCEPTION 'conflict scope mismatch';
 END IF;
 IF p_mode='statement' AND policy->>'action'='nothing' THEN RETURN jsonb_build_object('inserted',false,'outcome','duplicate','row',NULL); END IF;
 condition:='';sep:='';FOR key IN SELECT jsonb_array_elements_text(spec->'primary_columns') LOOP
  condition:=condition||sep||format('%I=(jsonb_populate_record(NULL::%s,$1)).%I',key,rel,key);sep:=' AND ';
 END LOOP;
 EXECUTE format('SELECT to_jsonb(t) FROM %s t WHERE %s%s',rel,condition,CASE WHEN p_mode IN ('usage_merge','statement') THEN ' FOR UPDATE' ELSE '' END) INTO found_row USING existing->'identity';
 IF found_row IS NULL THEN
  -- A retired global identity is a tombstone, not a missing billing operation.
  -- Never replay charges/admission because payload retention removed its row.
  IF existing->>'partition_state' NOT IN ('detaching','detached','archived','dropped') THEN RAISE EXCEPTION 'conflict payload disappeared or is not visible; re-observe without replay'; END IF;
  IF p_mode='statement' THEN RAISE EXCEPTION 'retired append identity: reconciliation required' USING ERRCODE='23505'; END IF;
  RETURN jsonb_build_object('inserted',false,'outcome','retained_identity','row',NULL);
 END IF;
 checked_identity:=partition_ops.lookup_conflict(p_family,p_conflict_index,p_row);
 IF checked_identity IS NULL OR checked_identity->'identity' IS DISTINCT FROM existing->'identity' THEN RAISE EXCEPTION 'global conflict identity changed; re-observe without replay'; END IF;
 IF p_mode='statement' THEN
  alias_name:=split_part(p_family,'.',2);qualified_condition:='';sep:='';
  FOR key IN SELECT jsonb_array_elements_text(spec->'primary_columns') LOOP
   qualified_condition:=qualified_condition||sep||format('%I.%I=(jsonb_populate_record(NULL::%s,$1)).%I',alias_name,key,rel,key);sep:=' AND ';
  END LOOP;
  assignments:=replace(policy->>'update_sql','__AF_P31__','($3->>''p31'')');
  condition:=replace(COALESCE(policy->>'where_sql','true'),'__AF_P31__','($3->>''p31'')');
  -- Original compiled SET/WHERE comes only from an authority-owned policy.
  -- EXCLUDED is the same fully typed/defaulted candidate as original INSERT.
  EXECUTE format('UPDATE %s AS %I SET %s FROM jsonb_populate_record(NULL::%s,$2) AS excluded WHERE %s AND (%s) RETURNING to_jsonb(%I.*)',rel,alias_name,assignments,rel,qualified_condition,condition,alias_name) INTO found_row USING existing->'identity',p_row,p_parameters;
  RETURN jsonb_build_object('inserted',false,'outcome',CASE WHEN found_row IS NULL THEN 'guard_conflict' ELSE 'merged' END,'row',found_row);
 END IF;
 IF p_mode='same_or_error' THEN
  IF EXISTS(SELECT 1 FROM jsonb_each(requested) x WHERE found_row->x.key IS DISTINCT FROM x.value) THEN RAISE EXCEPTION 'idempotent body conflict'; END IF;
  RETURN jsonb_build_object('inserted',false,'outcome','existing','row',found_row);
 END IF;
 IF p_family<>'gateway.usage_records' OR p_conflict_index<>'idx_usage_records_request_identity' THEN RAISE EXCEPTION 'usage merge requires the exact original tenant/request/service/type key'; END IF;
 -- Preserve usage_recorder::RAW_CONFLICT exactly: maxima, terminal status,
 -- first non-null error, immutable original pricing snapshot and price guard.
 IF NOT ((COALESCE(found_row->'metadata'->'pricing_snapshot'->>'price_book_schema','')='' AND COALESCE(p_row->'metadata'->'pricing_snapshot'->>'price_book_schema','')='') OR
  (found_row->'provider' IS NOT DISTINCT FROM p_row->'provider' AND found_row->'model'=p_row->'model' AND found_row->'metadata'->'pricing_snapshot'=p_row->'metadata'->'pricing_snapshot')) THEN
  RETURN jsonb_build_object('inserted',false,'outcome','pricing_conflict','row',NULL);
 END IF;
 EXECUTE format($sql$UPDATE %s SET
 input_tokens=GREATEST(input_tokens,(($2->>'input_tokens')::integer)),output_tokens=GREATEST(output_tokens,(($2->>'output_tokens')::integer)),
 input_cost_cents=GREATEST(input_cost_cents,(($2->>'input_cost_cents')::integer)),output_cost_cents=GREATEST(output_cost_cents,(($2->>'output_cost_cents')::integer)),
 status=CASE WHEN status IN('running','pending') AND $2->>'status'='success' THEN $2->>'status' ELSE status END,
 error_type=COALESCE(error_type,$2->>'error_type'),metadata=COALESCE(metadata,'{}'::jsonb)||COALESCE(NULLIF($2->'metadata','null'::jsonb),'{}'::jsonb)||CASE WHEN metadata->'pricing_snapshot' IS NOT NULL AND metadata->'pricing_snapshot'<>'null'::jsonb THEN jsonb_build_object('pricing_snapshot',metadata->'pricing_snapshot') ELSE '{}'::jsonb END
 WHERE %s RETURNING to_jsonb(%s.*)$sql$,rel,condition,rel) INTO found_row USING existing->'identity',p_row;
 RETURN jsonb_build_object('inserted',false,'outcome','merged','row',found_row);
END $$;
-- Authority grants USAGE on partition_ops and EXECUTE on the three functions
-- above only to measured roles already holding source INSERT, and only after
-- writer compatibility tests. Routines with owner-only INSERT can keep their
-- plain inserts: the row trigger enforces global keys without a new app grant.

REVOKE ALL ON FUNCTION partition_ops.statement_spec(text,text),partition_ops.column_types(text),partition_ops.require_payload(jsonb),partition_ops.write_json(text,jsonb,text,text,text,jsonb) FROM PUBLIC;
-- Authority explicitly grants schema USAGE and only family_enabled, column_types,
-- statement_spec, require_payload, write_json, write_spec, lookup_conflict to
-- original writer roles after all native writer fixtures pass. No DDL/dir grant.

-- INERT, unnumbered authority successor; install after write-compatibility.sql.
-- Exact native conflict contracts. Do not grant writer roles policy-table access.
-- Any changed/new statement fails closed until its policy is re-admitted.
-- Native runner enforces the original2s lock/60s statement budget.
-- platform-gateway-foundation/src/usage_recorder.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.usage_records','ON CONFLICT (tenant_id,(COALESCE(request_id,'''')),(COALESCE(service_id,'''')),(COALESCE(request_type,''''))) WHERE request_id IS NOT NULL AND request_id<>'''' DO UPDATE SET input_tokens=GREATEST(usage_records.input_tokens,EXCLUDED.input_tokens),output_tokens=GREATEST(usage_records.output_tokens,EXCLUDED.output_tokens),input_cost_cents=GREATEST(usage_records.input_cost_cents,EXCLUDED.input_cost_cents),output_cost_cents=GREATEST(usage_records.output_cost_cents,EXCLUDED.output_cost_cents),status=CASE WHEN usage_records.status IN (''running'',''pending'') AND EXCLUDED.status=''success'' THEN EXCLUDED.status ELSE usage_records.status END,error_type=COALESCE(usage_records.error_type,EXCLUDED.error_type),metadata=COALESCE(usage_records.metadata,''{}''::jsonb)||COALESCE(EXCLUDED.metadata,''{}''::jsonb)||CASE WHEN usage_records.metadata->''pricing_snapshot'' IS NOT NULL AND usage_records.metadata->''pricing_snapshot''<>''null''::jsonb THEN jsonb_build_object(''pricing_snapshot'',usage_records.metadata->''pricing_snapshot'') ELSE ''{}''::jsonb END WHERE (COALESCE(usage_records.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''' AND COALESCE(EXCLUDED.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''') OR (usage_records.provider IS NOT DISTINCT FROM EXCLUDED.provider AND usage_records.model=EXCLUDED.model AND usage_records.metadata->''pricing_snapshot''=EXCLUDED.metadata->''pricing_snapshot'') RETURNING (xmax=0) AS inserted,tenant_id,COALESCE(request_id,'''') AS request_id_key,COALESCE(service_id,'''') AS service_id_key,COALESCE(request_type,'''') AS request_type_key',NULL,'idx_usage_records_request_identity','update','input_tokens=GREATEST(usage_records.input_tokens,EXCLUDED.input_tokens),output_tokens=GREATEST(usage_records.output_tokens,EXCLUDED.output_tokens),input_cost_cents=GREATEST(usage_records.input_cost_cents,EXCLUDED.input_cost_cents),output_cost_cents=GREATEST(usage_records.output_cost_cents,EXCLUDED.output_cost_cents),status=CASE WHEN usage_records.status IN (''running'',''pending'') AND EXCLUDED.status=''success'' THEN EXCLUDED.status ELSE usage_records.status END,error_type=COALESCE(usage_records.error_type,EXCLUDED.error_type),metadata=COALESCE(usage_records.metadata,''{}''::jsonb)||COALESCE(EXCLUDED.metadata,''{}''::jsonb)||CASE WHEN usage_records.metadata->''pricing_snapshot'' IS NOT NULL AND usage_records.metadata->''pricing_snapshot''<>''null''::jsonb THEN jsonb_build_object(''pricing_snapshot'',usage_records.metadata->''pricing_snapshot'') ELSE ''{}''::jsonb END','(COALESCE(usage_records.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''' AND COALESCE(EXCLUDED.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''') OR (usage_records.provider IS NOT DISTINCT FROM EXCLUDED.provider AND usage_records.model=EXCLUDED.model AND usage_records.metadata->''pricing_snapshot''=EXCLUDED.metadata->''pricing_snapshot'')');
-- platform-gateway-foundation/src/usage_recorder.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.billing_events','ON CONFLICT(event_id) DO NOTHING',ARRAY['event_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/usage_stage_receipts.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.billing_events','ON CONFLICT(event_id) DO NOTHING RETURNING event_id',ARRAY['event_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/usage_stage_receipts.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.price_book_v2_accounting_checkpoints','ON CONFLICT(tenant_id,request_id,service_id,request_type,stage) DO UPDATE SET observation=EXCLUDED.observation,updated_at=CURRENT_TIMESTAMP',ARRAY['tenant_id','request_id','service_id','request_type','stage']::name[],NULL,'update','observation=EXCLUDED.observation,updated_at=CURRENT_TIMESTAMP',NULL);
-- platform-gateway-foundation/src/provider_batch_settlement.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.usage_records','ON CONFLICT DO NOTHING',NULL,NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/provider_batch_settlement.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.price_book_v2_accounting_checkpoints','ON CONFLICT DO NOTHING',NULL,NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_traces','ON CONFLICT (trace_id)
            DO UPDATE SET
                trace_family = EXCLUDED.trace_family,
                workflow_kind = EXCLUDED.workflow_kind,
                user_id = EXCLUDED.user_id,
                session_id = EXCLUDED.session_id,
                thread_id = EXCLUDED.thread_id,
                run_id = EXCLUDED.run_id,
                request_id = EXCLUDED.request_id,
                model_id = EXCLUDED.model_id,
                provider = EXCLUDED.provider,
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_traces.ended_at),
                total_latency_ms = GREATEST(agent_traces.total_latency_ms, EXCLUDED.total_latency_ms),
                first_token_latency_ms = GREATEST(agent_traces.first_token_latency_ms, EXCLUDED.first_token_latency_ms),
                input_tokens = GREATEST(agent_traces.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(agent_traces.output_tokens, EXCLUDED.output_tokens),
                total_tokens = GREATEST(agent_traces.total_tokens, EXCLUDED.total_tokens),
                total_cost_cents = GREATEST(agent_traces.total_cost_cents, EXCLUDED.total_cost_cents),
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()
            WHERE agent_traces.tenant_id = EXCLUDED.tenant_id
              AND ($31::text IS NULL OR agent_traces.user_id = $31)
            RETURNING trace_id',ARRAY['trace_id']::name[],NULL,'update','trace_family = EXCLUDED.trace_family,
                workflow_kind = EXCLUDED.workflow_kind,
                user_id = EXCLUDED.user_id,
                session_id = EXCLUDED.session_id,
                thread_id = EXCLUDED.thread_id,
                run_id = EXCLUDED.run_id,
                request_id = EXCLUDED.request_id,
                model_id = EXCLUDED.model_id,
                provider = EXCLUDED.provider,
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_traces.ended_at),
                total_latency_ms = GREATEST(agent_traces.total_latency_ms, EXCLUDED.total_latency_ms),
                first_token_latency_ms = GREATEST(agent_traces.first_token_latency_ms, EXCLUDED.first_token_latency_ms),
                input_tokens = GREATEST(agent_traces.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(agent_traces.output_tokens, EXCLUDED.output_tokens),
                total_tokens = GREATEST(agent_traces.total_tokens, EXCLUDED.total_tokens),
                total_cost_cents = GREATEST(agent_traces.total_cost_cents, EXCLUDED.total_cost_cents),
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()','agent_traces.tenant_id = EXCLUDED.tenant_id
              AND (__AF_P31__::text IS NULL OR agent_traces.user_id = __AF_P31__)');
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_spans','ON CONFLICT (span_id)
            DO UPDATE SET
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)
            WHERE agent_trace_spans.trace_id = EXCLUDED.trace_id
            RETURNING span_id',ARRAY['span_id']::name[],NULL,'update','status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)','agent_trace_spans.trace_id = EXCLUDED.trace_id');
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_events','ON CONFLICT (trace_id, sequence_no)
            DO UPDATE SET
                span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted
            RETURNING event_id',ARRAY['trace_id','sequence_no']::name[],NULL,'update','span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted',NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_traces','ON CONFLICT (trace_id) DO NOTHING RETURNING trace_id',ARRAY['trace_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_spans','ON CONFLICT (span_id) DO NOTHING',ARRAY['span_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_events','ON CONFLICT (trace_id, sequence_no) DO NOTHING',ARRAY['trace_id','sequence_no']::name[],NULL,'nothing',NULL,NULL);

-- Owner-only retention barrier, archive evidence and interrupted detach recovery.
-- DETACH CONCURRENTLY / FINALIZE are separate nontransactional native-authority
-- commands; no SECURITY DEFINER function can smuggle app DDL into this protocol.
-- Native runner enforces the original2s lock/60s statement budget.
CREATE FUNCTION partition_ops.seal_acl(p_relation oid) RETURNS jsonb LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
DECLARE a record; restore jsonb:='[]'; principal text;
BEGIN
 PERFORM partition_ops.assert_owner();
 FOR a IN SELECT x.* FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) x WHERE c.oid=p_relation AND x.grantee<>c.relowner LOOP
  principal:=CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(a.grantee)) END;
  restore:=restore||jsonb_build_array(format('GRANT %s ON %s TO %s%s',a.privilege_type,p_relation::regclass,principal,CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END));
 END LOOP;
 FOR a IN SELECT DISTINCT x.grantee FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) x WHERE c.oid=p_relation AND x.grantee<>c.relowner LOOP
  EXECUTE format('REVOKE ALL ON %s FROM %s',p_relation::regclass,CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(a.grantee)) END);
 END LOOP;
 FOR a IN SELECT c.attname,x.* FROM pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) x WHERE c.attrelid=p_relation AND c.attnum>0 LOOP
  principal:=CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(a.grantee)) END;
  restore:=restore||jsonb_build_array(format('GRANT %s(%I) ON %s TO %s%s',a.privilege_type,a.attname,p_relation::regclass,principal,CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END));
  EXECUTE format('REVOKE ALL(%I) ON %s FROM %s',a.attname,p_relation::regclass,principal);
 END LOOP;
 RETURN restore;
END $$;
CREATE FUNCTION partition_ops.retention_begin(p_family text,p_relation text,p_cutoff timestamptz,p_evidence jsonb)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp SET TimeZone='UTC' AS $$
DECLARE f partition_ops.families; p partition_ops.partitions; c record; unsafe boolean; join_sql text; n integer; src name[]; dst name[]; hash_field text; sealed_rows bigint;
BEGIN
 PERFORM partition_ops.assert_owner();
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 SELECT * INTO STRICT p FROM partition_ops.partitions WHERE family=p_family AND relation_name=p_relation FOR UPDATE;
 IF p_cutoff IS NULL OR f.phase<>'active' OR p.state<>'active' OR p.upper_at>p_cutoff OR p.upper_at>=clock_timestamp() OR to_regclass(p_relation) IS DISTINCT FROM p.relation_oid THEN RAISE EXCEPTION 'live, retained or changed partition refused'; END IF;
 FOR hash_field IN SELECT unnest(ARRAY['authority_snapshot_sha256','pitr_restore_drill_sha256','role_rls_acceptance_sha256','compatibility_sha256','retention_approval_sha256']) LOOP
  IF COALESCE(p_evidence->>hash_field,'') !~ '^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'authority/PITR/role/RLS/compatibility/retention evidence required'; END IF;
 END LOOP;
 IF p_evidence->>'compatibility_sha256' IS DISTINCT FROM f.compatibility_sha256 OR p_evidence->>'family' IS DISTINCT FROM p_family OR p_evidence->>'relation' IS DISTINCT FROM p_relation OR (p_evidence->>'relation_oid')::oid IS DISTINCT FROM p.relation_oid THEN RAISE EXCEPTION 'retention evidence scope mismatch'; END IF;
 -- UPDATE/INSERT guards hold KEY SHARE on this state row until their commit.
 -- Taking FOR UPDATE above drains them; changing state prevents subsequent
 -- payload resurrection and new FK references between seal and detach.
 UPDATE partition_ops.partitions SET state='detaching',retention_evidence=p_evidence,sealed_acl=partition_ops.seal_acl(p.relation_oid) WHERE relation_oid=p.relation_oid;
 EXECUTE format('SELECT EXISTS(SELECT 1 FROM %s t WHERE t.%I IS NULL OR (%s) IS NOT TRUE)',p.relation_oid::regclass,f.clock_column,f.safe_retention_predicate) INTO unsafe;
 IF unsafe THEN RAISE EXCEPTION 'NULL-time, active, unresolved, unsettled or held work prevents retention'; END IF;
 -- Retaining a global key directory does not excuse a dangling payload FK.
 -- All inbound references outside the table being dropped must drain first.
 -- Actual catalog FKs include nonpartitioned consumers and detached archives.
 FOR c IN SELECT * FROM pg_constraint WHERE contype='f' AND confrelid=f.registry_oid AND conparentid=0 AND conname<>'af_directory_identity' ORDER BY oid LOOP
  SELECT array_agg(a.attname ORDER BY k.ord) INTO src FROM unnest(c.conkey) WITH ORDINALITY k(n,ord) JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.n;
  SELECT array_agg(a.attname ORDER BY k.ord) INTO dst FROM unnest(c.confkey) WITH ORDINALITY k(n,ord) JOIN pg_attribute a ON a.attrelid=c.confrelid AND a.attnum=k.n;
  join_sql:='';FOR n IN 1..cardinality(src) LOOP join_sql:=join_sql||CASE WHEN n>1 THEN ' AND ' ELSE '' END||format('t.%I=k.%I',src[n],dst[n]); END LOOP;
  EXECUTE format('SELECT EXISTS(SELECT 1 FROM %s t JOIN %s k ON %s WHERE k.af_partition=$1 AND t.tableoid<>$2)',c.conrelid::regclass,f.registry_oid::regclass,join_sql) INTO unsafe USING p.relation_oid::text,p.relation_oid;
  IF unsafe THEN RAISE EXCEPTION 'referencing payload prevents detach; archive/drop children first (FK %)',c.conname; END IF;
 END LOOP;
 IF EXISTS(SELECT 1 FROM pg_inherits i JOIN pg_class x ON x.oid=i.inhrelid WHERE i.inhparent=f.parent_oid AND pg_get_expr(x.relpartbound,x.oid)='DEFAULT') THEN RAISE EXCEPTION 'DEFAULT partition prevents concurrent detach'; END IF;
 EXECUTE format('SELECT count(*) FROM %s',p.relation_oid::regclass) INTO sealed_rows;
 UPDATE partition_ops.partitions SET retention_evidence=p_evidence||jsonb_build_object('rows',sealed_rows,'sealed_at',clock_timestamp()) WHERE relation_oid=p.relation_oid;
 RETURN partition_ops.detach_status(p_family,p_relation);
END $$;
CREATE FUNCTION partition_ops.detach_status(p_family text,p_relation text) RETURNS jsonb
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; p partition_ops.partitions; pending boolean; attached boolean; command text;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 SELECT * INTO STRICT p FROM partition_ops.partitions WHERE family=p_family AND relation_name=p_relation FOR UPDATE;
 IF p.state NOT IN('detaching','detached','archived','dropped') THEN RAISE EXCEPTION 'retention barrier required'; END IF;
 SELECT true,inhdetachpending INTO attached,pending FROM pg_inherits WHERE inhrelid=p.relation_oid AND inhparent=f.parent_oid;
 IF attached IS NOT TRUE AND p.state='detaching' THEN UPDATE partition_ops.partitions SET state='detached' WHERE relation_oid=p.relation_oid; p.state:='detached'; END IF;
 IF attached AND p.state<>'detaching' THEN RAISE EXCEPTION 'retention catalog state mismatch'; END IF;
 command:=CASE WHEN attached AND pending THEN format('ALTER TABLE %s DETACH PARTITION %s FINALIZE',f.parent_oid::regclass,p.relation_oid::regclass)
  WHEN attached THEN format('ALTER TABLE %s DETACH PARTITION %s CONCURRENTLY',f.parent_oid::regclass,p.relation_oid::regclass) ELSE NULL END;
 RETURN to_jsonb(p)||jsonb_build_object('transactional',false,'next_authority_sql',command,'identity_directory_retained',true,'drop_ready',p.state='archived');
END $$;
CREATE FUNCTION partition_ops.record_archive(p_family text,p_relation text,p_receipt jsonb) RETURNS jsonb
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE p partition_ops.partitions; field text;
BEGIN
 PERFORM partition_ops.assert_owner(); PERFORM partition_ops.detach_status(p_family,p_relation);
 SELECT * INTO STRICT p FROM partition_ops.partitions WHERE family=p_family AND relation_name=p_relation FOR UPDATE;
 IF p.state NOT IN('detached','archived') THEN RAISE EXCEPTION 'detached immutable partition required'; END IF;
 FOR field IN SELECT unnest(ARRAY['archive_sha256','archive_manifest_sha256','restore_validation_sha256','source_snapshot_sha256']) LOOP
  IF COALESCE(p_receipt->>field,'') !~ '^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'verified archive and restore receipt required'; END IF;
 END LOOP;
 IF p_receipt->>'family' IS DISTINCT FROM p_family OR p_receipt->>'relation' IS DISTINCT FROM p_relation OR (p_receipt->>'relation_oid')::oid IS DISTINCT FROM p.relation_oid OR COALESCE((p_receipt->>'verified')::boolean,false) IS NOT TRUE
 OR p_receipt->>'source_snapshot_sha256' IS DISTINCT FROM p.retention_evidence->>'authority_snapshot_sha256' OR COALESCE((p_receipt->>'rows')::bigint,-1)<0 OR (p_receipt->>'rows')::bigint IS DISTINCT FROM (p.retention_evidence->>'rows')::bigint THEN RAISE EXCEPTION 'archive receipt scope/source mismatch'; END IF;
 IF p.archive_receipt IS NOT NULL AND p.archive_receipt<>p_receipt THEN RAISE EXCEPTION 'archive receipt replacement refused'; END IF;
 UPDATE partition_ops.partitions SET state='archived',archive_receipt=p_receipt WHERE relation_oid=p.relation_oid;
 RETURN partition_ops.detach_status(p_family,p_relation);
END $$;
CREATE FUNCTION partition_ops.drop_archived(p_family text,p_relation text,p_archive_manifest_sha256 text)
RETURNS jsonb LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
DECLARE p partition_ops.partitions;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT p FROM partition_ops.partitions WHERE family=p_family AND relation_name=p_relation FOR UPDATE;
 IF p.state='dropped' AND p.archive_receipt->>'archive_manifest_sha256'=p_archive_manifest_sha256 THEN RETURN to_jsonb(p); END IF;
 IF p.state<>'archived' OR p.archive_receipt->>'archive_manifest_sha256' IS DISTINCT FROM p_archive_manifest_sha256 OR to_regclass(p_relation) IS DISTINCT FROM p.relation_oid OR EXISTS(SELECT 1 FROM pg_inherits WHERE inhrelid=p.relation_oid) THEN RAISE EXCEPTION 'verified detached archive required'; END IF;
 -- No CASCADE and no per-row DELETE. Keep minimal typed global key directories
 -- indefinitely; old idempotency keys never become fresh billable operations.
 EXECUTE format('DROP TABLE %s',p.relation_oid::regclass);
 UPDATE partition_ops.partitions SET state='dropped' WHERE relation_oid=p.relation_oid;
 RETURN (SELECT to_jsonb(x) FROM partition_ops.partitions x WHERE relation_oid=p.relation_oid);
END $$;
CREATE FUNCTION partition_ops.reattach(p_family text,p_relation text) RETURNS jsonb LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
DECLARE f partition_ops.families; p partition_ops.partitions; grant_sql text;
BEGIN
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family;
 PERFORM partition_ops.detach_status(p_family,p_relation);
 SELECT * INTO STRICT p FROM partition_ops.partitions WHERE family=p_family AND relation_name=p_relation FOR UPDATE;
 IF p.state NOT IN('detached','archived') OR to_regclass(p_relation) IS DISTINCT FROM p.relation_oid THEN RAISE EXCEPTION 'reattach requires preserved detached bytes'; END IF;
 EXECUTE format('ALTER TABLE %s ATTACH PARTITION %s FOR VALUES FROM (%s) TO (%L)',f.parent_oid::regclass,p.relation_oid::regclass,CASE WHEN p.lower_at IS NULL THEN 'MINVALUE' ELSE quote_literal(p.lower_at) END,p.upper_at);
 FOR grant_sql IN SELECT jsonb_array_elements_text(p.sealed_acl) LOOP EXECUTE grant_sql; END LOOP;
 UPDATE partition_ops.partitions SET state='active',archive_history=archive_history||jsonb_build_array(jsonb_build_object('retention_evidence',retention_evidence,'archive_receipt',archive_receipt,'reattached_at',clock_timestamp())),archive_receipt=NULL,retention_evidence=NULL,sealed_acl=NULL WHERE relation_oid=p.relation_oid;
 RETURN partition_ops.status(p_family);
END $$;
-- Close all functions/tables to PUBLIC, including helpers/trigger functions.
-- The native authority separately grants exact invoker write entrypoints only.
DO $revoke$
DECLARE f record;
BEGIN
 REVOKE ALL ON ALL TABLES IN SCHEMA partition_ops FROM PUBLIC;
 FOR f IN SELECT p.oid::regprocedure signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='partition_ops' LOOP
  EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',f.signature);
 END LOOP;
END $revoke$;

-- INERT independent authority successor, after expand/write/migrate/retention.
-- Exact legacy bodies below remain selected until this family's explicit expand.
-- Native runner enforces the original2s lock/60s statement budget.
CREATE OR REPLACE FUNCTION gateway.cleanup_old_usage_records(retention_days integer DEFAULT 30) RETURNS integer
    LANGUAGE plpgsql
    AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    IF CASE WHEN current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN partition_ops.owner_family_enabled('gateway.usage_records') ELSE partition_ops.family_enabled('gateway.usage_records') END THEN RETURN 0; END IF;
    DELETE FROM usage_records
    WHERE created_at < CURRENT_TIMESTAMP - (retention_days || ' days')::INTERVAL;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$;
CREATE OR REPLACE FUNCTION knowledge.prune_kb_document_progress_events() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'knowledge', 'pg_catalog'
    AS $$
DECLARE
    retained_floor BIGINT;
BEGIN
    IF CASE WHEN current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN partition_ops.owner_family_enabled('knowledge.kb_document_progress_events') ELSE partition_ops.family_enabled('knowledge.kb_document_progress_events') END THEN RETURN NEW; END IF;
    -- Seven days is longer than the browser reconnect contract. Run the indexed
    -- age prune on each new transition so an inactive dataset needs no scheduler.
    DELETE FROM knowledge.kb_document_progress_events
    WHERE dataset_id = NEW.dataset_id
      AND created_at < NOW() - INTERVAL '7 days';

    -- The age window can still be noisy during bulk ingestion. Amortize the
    -- per-dataset row cap to one indexed check per 128 global events.
    IF MOD(NEW.event_sequence, 128) = 0 THEN
        SELECT MIN(recent.event_sequence)
        INTO retained_floor
        FROM (
            SELECT event_sequence
            FROM knowledge.kb_document_progress_events
            WHERE dataset_id = NEW.dataset_id
            ORDER BY event_sequence DESC
            LIMIT 10000
        ) AS recent;

        IF retained_floor IS NOT NULL THEN
            DELETE FROM knowledge.kb_document_progress_events
            WHERE dataset_id = NEW.dataset_id
              AND event_sequence < retained_floor;
        END IF;
    END IF;

    RETURN NEW;
END;
$$;
DO $grants$
DECLARE prefix text; role_name text; candidate text; can_read boolean; can_write boolean; family record;
BEGIN
 PERFORM partition_ops.assert_owner();prefix:=left(current_user,length(current_user)-5);
 REVOKE ALL ON ALL TABLES IN SCHEMA partition_ops FROM PUBLIC;
 REVOKE ALL ON ALL FUNCTIONS IN SCHEMA partition_ops FROM PUBLIC;
 FOREACH candidate IN ARRAY ARRAY['gateway','runtime','capability_worker','knowledge_api','knowledge_worker'] LOOP
  role_name:=prefix||candidate;
  IF NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname=role_name AND NOT rolsuper AND NOT rolbypassrls) THEN RAISE EXCEPTION 'expected bounded native role absent'; END IF;
  can_read:=false;can_write:=false;
  FOR family IN SELECT f.family FROM partition_ops.families f LOOP
   IF to_regclass(family.family) IS NOT NULL THEN
    can_read:=can_read OR has_table_privilege(role_name,family.family,'SELECT') OR has_table_privilege(role_name,family.family,'DELETE');
    can_write:=can_write OR has_table_privilege(role_name,family.family,'INSERT');
   END IF;
  END LOOP;
  IF can_read OR can_write THEN
   EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',role_name);
   EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',role_name);
  END IF;
  IF can_write THEN
   EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.column_types(text),partition_ops.statement_spec(text,text),partition_ops.require_payload(jsonb),partition_ops.write_json(text,jsonb,text,text,text,jsonb),partition_ops.write_spec(text),partition_ops.lookup_conflict(text,text,jsonb) TO %I',role_name);
  END IF;
 END LOOP;
END $grants$;

CREATE FUNCTION partition_ops.schema_admitted() RETURNS boolean LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); valid boolean;
BEGIN
 PERFORM partition_ops.assert_owner();
 IF session_user<>ALL(ARRAY[prefix||'gateway',prefix||'runtime',prefix||'capability_worker',prefix||'knowledge_api',prefix||'knowledge_worker']) THEN RETURN false; END IF;
 IF NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname=session_user AND rolcanlogin AND NOT (rolsuper OR rolinherit OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)) OR EXISTS(SELECT 1 FROM pg_auth_members m JOIN pg_roles r ON r.oid=m.member WHERE r.rolname=session_user) THEN RETURN false; END IF;
 SELECT (SELECT count(*)=22 AND COALESCE(bool_and(clock_column IS NOT NULL AND cadence IN('day','month') AND phase IN('registered','expanded','backfilled','validated','active')),false) FROM partition_ops.families WHERE family=ANY(ARRAY['gateway.usage_records','gateway.request_traces','gateway.audit_logs','gateway.billing_events','gateway.agent_trace_spans','gateway.agent_trace_events','gateway.tool_audit_log','gateway.login_audit','gateway.service_health_records','assistant.assistant_runtime_items','assistant.assistant_audit_events','assistant.assistant_capability_events','knowledge.kb_document_progress_events','assistant.assistant_runtime_model_calls','assistant.assistant_runtime_model_leases','gateway.knowledge_model_calls','gateway.task_reconciliation_audit','gateway.price_book_v2_activation_audit','gateway.price_book_v2_import_audit','gateway.price_book_v2_usage_receipts','gateway.price_book_v2_accounting_checkpoints','gateway.agent_traces']))
 AND (SELECT count(*)=22 FROM partition_ops.families)
 AND (SELECT count(*)=5 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='partition_ops' AND c.relkind='r' AND c.relname=ANY(ARRAY['families','partitions','foreign_keys','statement_policies','operator_approvals']) AND pg_get_userbyid(c.relowner)=current_user)
 AND NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a WHERE n.nspname='partition_ops' AND a.grantee<>c.relowner)
 AND NOT EXISTS(SELECT 1 FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL aclexplode(a.attacl) x WHERE n.nspname='partition_ops' AND x.grantee<>c.relowner)
 AND (SELECT count(*)=1 FROM pg_namespace WHERE nspname='partition_ops' AND pg_get_userbyid(nspowner)=current_user)
 AND NOT EXISTS(SELECT 1 FROM pg_namespace n CROSS JOIN LATERAL aclexplode(COALESCE(n.nspacl,acldefault('n',n.nspowner))) a WHERE n.nspname='partition_ops' AND a.grantee<>n.nspowner AND (a.privilege_type<>'USAGE' OR a.is_grantable OR pg_get_userbyid(a.grantee)<>ALL(ARRAY[prefix||'gateway',prefix||'runtime',prefix||'capability_worker',prefix||'knowledge_api',prefix||'knowledge_worker'])))
 AND NOT EXISTS(SELECT 1 FROM (VALUES ('assert_owner','',false,ARRAY[]::text[]),('family_enabled','p_family text',true,ARRAY['capability_worker','gateway','knowledge_api','knowledge_worker','runtime']::text[]),('identity','p_columns name[], p_row jsonb',false,ARRAY[]::text[]),('projection','p_columns name[], p_row jsonb',false,ARRAY[]::text[]),('claim_row','p_family text, p_row jsonb, p_identity jsonb, p_moving boolean',false,ARRAY[]::text[]),('row_guard','',true,ARRAY[]::text[]),('expand','p_family text, p_boundary timestamp with time zone, p_admission_sha256 text',false,ARRAY[]::text[]),('owner_family_enabled','p_family text',false,ARRAY[]::text[]),('approve_operator_stage','p_family text, p_stage text, p_revision bigint, p_scope_sha256 text, p_evidence jsonb',false,ARRAY[]::text[]),('require_operator_approval','p_family text, p_stage text, p_revision bigint, p_scope_sha256 text',false,ARRAY[]::text[]),('status','p_family text',false,ARRAY[]::text[]),('backfill','p_family text, p_expected_revision bigint, p_limit integer',false,ARRAY[]::text[]),('validate_legacy','p_family text, p_expected_revision bigint',false,ARRAY[]::text[]),('capture_foreign_keys','p_family text',false,ARRAY[]::text[]),('reference_guard','',true,ARRAY[]::text[]),('rebind_fk','p_oid oid, p_step text',false,ARRAY[]::text[]),('prepare','p_family text, p_first timestamp with time zone, p_count integer',false,ARRAY[]::text[]),('cutover','p_family text, p_revision bigint, p_compatibility_sha256 text',false,ARRAY[]::text[]),('legacy_index_sql','p_family text',false,ARRAY[]::text[]),('write_spec','p_family text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('lookup_conflict','p_family text, p_index text, p_row jsonb',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('statement_spec','p_family text, p_tail text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('column_types','p_family text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('require_payload','p_result jsonb',false,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('write_json','p_family text, p_row jsonb, p_mode text, p_conflict_index text, p_statement text, p_parameters jsonb',false,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('seal_acl','p_relation oid',false,ARRAY[]::text[]),('retention_begin','p_family text, p_relation text, p_cutoff timestamp with time zone, p_evidence jsonb',false,ARRAY[]::text[]),('detach_status','p_family text, p_relation text',false,ARRAY[]::text[]),('record_archive','p_family text, p_relation text, p_receipt jsonb',false,ARRAY[]::text[]),('drop_archived','p_family text, p_relation text, p_archive_manifest_sha256 text',false,ARRAY[]::text[]),('reattach','p_family text, p_relation text',false,ARRAY[]::text[]),('schema_admitted','',true,ARRAY['capability_worker','gateway','knowledge_api','knowledge_worker','runtime']::text[])) e(name,args,definer,roles) LEFT JOIN pg_proc p ON p.pronamespace='partition_ops'::regnamespace AND p.proname=e.name AND pg_get_function_identity_arguments(p.oid)=e.args WHERE p.oid IS NULL OR pg_get_userbyid(p.proowner)<>current_user OR p.prosecdef<>e.definer OR NOT COALESCE(p.proconfig@>ARRAY['search_path=pg_catalog, pg_temp']::text[],false) OR EXISTS(SELECT 1 FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a WHERE a.grantee<>p.proowner AND (a.privilege_type<>'EXECUTE' OR a.is_grantable OR NOT pg_get_userbyid(a.grantee)=ANY(ARRAY(SELECT prefix||r FROM unnest(e.roles) r)))) OR EXISTS(SELECT 1 FROM unnest(e.roles) r WHERE NOT has_function_privilege(prefix||r,p.oid,'EXECUTE')))
 AND NOT EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='partition_ops' AND NOT EXISTS(SELECT 1 FROM (VALUES ('assert_owner','',false,ARRAY[]::text[]),('family_enabled','p_family text',true,ARRAY['capability_worker','gateway','knowledge_api','knowledge_worker','runtime']::text[]),('identity','p_columns name[], p_row jsonb',false,ARRAY[]::text[]),('projection','p_columns name[], p_row jsonb',false,ARRAY[]::text[]),('claim_row','p_family text, p_row jsonb, p_identity jsonb, p_moving boolean',false,ARRAY[]::text[]),('row_guard','',true,ARRAY[]::text[]),('expand','p_family text, p_boundary timestamp with time zone, p_admission_sha256 text',false,ARRAY[]::text[]),('owner_family_enabled','p_family text',false,ARRAY[]::text[]),('approve_operator_stage','p_family text, p_stage text, p_revision bigint, p_scope_sha256 text, p_evidence jsonb',false,ARRAY[]::text[]),('require_operator_approval','p_family text, p_stage text, p_revision bigint, p_scope_sha256 text',false,ARRAY[]::text[]),('status','p_family text',false,ARRAY[]::text[]),('backfill','p_family text, p_expected_revision bigint, p_limit integer',false,ARRAY[]::text[]),('validate_legacy','p_family text, p_expected_revision bigint',false,ARRAY[]::text[]),('capture_foreign_keys','p_family text',false,ARRAY[]::text[]),('reference_guard','',true,ARRAY[]::text[]),('rebind_fk','p_oid oid, p_step text',false,ARRAY[]::text[]),('prepare','p_family text, p_first timestamp with time zone, p_count integer',false,ARRAY[]::text[]),('cutover','p_family text, p_revision bigint, p_compatibility_sha256 text',false,ARRAY[]::text[]),('legacy_index_sql','p_family text',false,ARRAY[]::text[]),('write_spec','p_family text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('lookup_conflict','p_family text, p_index text, p_row jsonb',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('statement_spec','p_family text, p_tail text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('column_types','p_family text',true,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('require_payload','p_result jsonb',false,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('write_json','p_family text, p_row jsonb, p_mode text, p_conflict_index text, p_statement text, p_parameters jsonb',false,ARRAY['gateway','knowledge_api','knowledge_worker']::text[]),('seal_acl','p_relation oid',false,ARRAY[]::text[]),('retention_begin','p_family text, p_relation text, p_cutoff timestamp with time zone, p_evidence jsonb',false,ARRAY[]::text[]),('detach_status','p_family text, p_relation text',false,ARRAY[]::text[]),('record_archive','p_family text, p_relation text, p_receipt jsonb',false,ARRAY[]::text[]),('drop_archived','p_family text, p_relation text, p_archive_manifest_sha256 text',false,ARRAY[]::text[]),('reattach','p_family text, p_relation text',false,ARRAY[]::text[]),('schema_admitted','',true,ARRAY['capability_worker','gateway','knowledge_api','knowledge_worker','runtime']::text[])) e(name,args,definer,roles) WHERE e.name=p.proname AND e.args=pg_get_function_identity_arguments(p.oid)))
 INTO valid;
 RETURN COALESCE(valid,false);
END $$;
REVOKE ALL ON ALL TABLES IN SCHEMA partition_ops FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA partition_ops FROM PUBLIC;
DO $partition_acl$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',prefix||'capability_worker');
 EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',prefix||'gateway');
 EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',prefix||'runtime');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',prefix||'capability_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',prefix||'runtime');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_spec(text) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_spec(text) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_spec(text) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.lookup_conflict(text, text, jsonb) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.lookup_conflict(text, text, jsonb) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.lookup_conflict(text, text, jsonb) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.statement_spec(text, text) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.statement_spec(text, text) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.statement_spec(text, text) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.column_types(text) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.column_types(text) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.column_types(text) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.require_payload(jsonb) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.require_payload(jsonb) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.require_payload(jsonb) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_json(text, jsonb, text, text, text, jsonb) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_json(text, jsonb, text, text, text, jsonb) TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.write_json(text, jsonb, text, text, text, jsonb) TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.schema_admitted() TO %I',prefix||'capability_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.schema_admitted() TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.schema_admitted() TO %I',prefix||'knowledge_api');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.schema_admitted() TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.schema_admitted() TO %I',prefix||'runtime');
END $partition_acl$;
