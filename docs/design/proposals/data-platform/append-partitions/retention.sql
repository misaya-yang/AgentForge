-- Owner-only retention barrier, archive evidence and interrupted detach recovery.
-- DETACH CONCURRENTLY / FINALIZE are separate nontransactional native-authority
-- commands; no SECURITY DEFINER function can smuggle app DDL into this protocol.
SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='60s';
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
