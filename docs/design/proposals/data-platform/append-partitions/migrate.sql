-- Independent authority functions. Every call is one bounded, resumable owner
-- transaction. The native authority must load expand.sql before this proposal.
SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='60s';
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
 PERFORM partition_ops.assert_owner(); SELECT * INTO STRICT f FROM partition_ops.families WHERE family=p_family FOR UPDATE;
 IF f.phase='active' AND f.compatibility_sha256=p_compatibility_sha256 THEN RETURN partition_ops.status(p_family); END IF;
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
