-- INERT, INDEPENDENT, UNNUMBERED AUTHORITY SUCCESSOR PROPOSAL. Supersedes the
-- audit-only candidate after authority admission; never apply both expansions.
-- No <=029 migration or frozen authority fingerprint is modified.
SET LOCAL lock_timeout='2s';
SET LOCAL statement_timeout='60s';
CREATE SCHEMA partition_ops AUTHORIZATION ai_gateway_owner;
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
DECLARE f partition_ops.families; old_key jsonb; new_key jsonb; old_time timestamptz; new_time timestamptz; moving bigint; exists_key boolean; physical text; patch jsonb; assignments text;
BEGIN
 SELECT * INTO STRICT f FROM partition_ops.families WHERE family=TG_ARGV[0];
 IF TG_LEVEL<>'ROW' OR TG_NARGS<>1 OR NOT (TG_RELID=f.source_oid OR TG_RELID=f.parent_oid OR EXISTS(SELECT 1 FROM pg_partition_ancestors(TG_RELID) a WHERE a.relid=f.parent_oid)) THEN RAISE EXCEPTION 'invalid global identity trigger'; END IF;
 IF TG_OP<>'INSERT' THEN old_key:=partition_ops.identity(f.primary_columns,to_jsonb(OLD)); old_time:=COALESCE((to_jsonb(OLD)->>f.clock_column)::timestamptz,'-infinity'::timestamptz); END IF;
 IF TG_OP<>'DELETE' THEN new_key:=partition_ops.identity(f.primary_columns,to_jsonb(NEW)); new_time:=COALESCE((to_jsonb(NEW)->>f.clock_column)::timestamptz,'-infinity'::timestamptz); END IF;
 IF TG_WHEN='AFTER' THEN
  IF TG_OP='DELETE' THEN
   -- Partition-key UPDATE is implemented as DELETE+INSERT. Its BEFORE UPDATE
   -- already moved the directory identity/time; do not fire DELETE FK actions.
   EXECUTE format('DELETE FROM %s WHERE af_identity=$1 AND af_time=$2 AND af_moving_xid IS NULL',f.registry_oid::regclass) USING old_key,old_time;
  ELSE
   IF TG_OP='INSERT' THEN
    -- Reserve only after the real INSERT's RLS WITH CHECK, NOT NULL and
    -- CHECK constraints. A BEFORE unique error would let an invoker catch a
    -- collision before those original checks and accidentally bypass them.
    EXECUTE format('SELECT af_moving_xid FROM %s WHERE af_identity=$1',f.registry_oid::regclass) INTO moving USING new_key;
    IF moving IS DISTINCT FROM txid_current() THEN PERFORM partition_ops.claim_row(f.family,to_jsonb(NEW),new_key); END IF;
   END IF;
   EXECUTE format('UPDATE %s SET af_moving_xid=NULL WHERE af_identity=$1 AND af_moving_xid=txid_current()',f.registry_oid::regclass) USING new_key;
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
  patch:=partition_ops.projection(f.key_columns,to_jsonb(NEW))||jsonb_build_object('af_identity',new_key,'af_time',new_time,'af_partition',physical,'af_moving_xid',CASE WHEN old_key<>new_key OR old_time<>new_time THEN txid_current() ELSE NULL END);
  SELECT string_agg(format('%1$I=(jsonb_populate_record(NULL::%2$s,$1)).%1$I',a.attname,f.registry_oid::regclass),',') INTO assignments FROM pg_attribute a WHERE a.attrelid=f.registry_oid AND a.attnum>0 AND NOT a.attisdropped;
  EXECUTE format('UPDATE %s SET %s WHERE af_identity=$2',f.registry_oid::regclass,assignments) USING patch,old_key;
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
 IF p_boundary IS NULL OR p_boundary<>date_trunc(f.cadence,p_boundary,'UTC') THEN RAISE EXCEPTION 'UTC partition boundary required'; END IF;
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
 a.attname IN('tenant_id','user_id') OR EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid=source AND i.indisunique AND a.attnum=ANY(i.indkey)) OR EXISTS(SELECT 1 FROM pg_index i JOIN pg_depend d ON d.classid='pg_class'::regclass AND d.objid=i.indexrelid WHERE i.indrelid=source AND i.indisunique AND d.refobjid=source AND d.refobjsubid=a.attnum));
 IF EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid=source AND attname=ANY(f.key_columns) AND attgenerated<>'') THEN RAISE EXCEPTION 'generated unique-key evolution needs an explicit successor'; END IF;
 parent_name:=format('%I.%I',split_part(p_family,'.',1),split_part(p_family,'.',2)||'_af_next');
 registry_name:=format('partition_ops.%I',replace(p_family,'.','_')||'_keys');
 EXECUTE format('CREATE TABLE %s AS SELECT * FROM %s WITH NO DATA',registry_name,source);
 EXECUTE format('ALTER TABLE %s ADD COLUMN af_identity jsonb PRIMARY KEY, ADD COLUMN af_time timestamptz NOT NULL, ADD COLUMN af_partition text NOT NULL, ADD COLUMN af_moving_xid bigint',registry_name);
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
