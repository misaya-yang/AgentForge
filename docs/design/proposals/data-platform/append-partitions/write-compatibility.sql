-- Caller-invoker compatibility entry: original INSERT/UPDATE/SELECT privileges
-- and FORCE RLS still apply to the real payload relation. Never grant an app
-- direct directory access or execute the invoker writer as a universal definer.
SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='60s';
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
 IF existing IS NOT NULL AND ((existing ? 'tenant_id') AND existing->'tenant_id' IS DISTINCT FROM p_row->'tenant_id' OR p_family IN ('gateway.agent_trace_spans','gateway.agent_trace_events') AND NOT EXISTS(SELECT 1 FROM gateway.agent_traces t WHERE t.trace_id=(existing->>'trace_id')::uuid AND t.tenant_id=current_setting('app.tenant_id',true))) THEN
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
