-- READ-ONLY owner assertions after all 22 independently admitted family cutovers.
-- Not a migration; never run against an unowned database or infer a PITR pass.
SELECT partition_ops.assert_owner();
DO $checks$
DECLARE f partition_ops.families; i record; original_tail text; copied_tail text; missing bigint;
BEGIN
 IF current_setting('agentforge.partition_acceptance',true) IS DISTINCT FROM 'isolated-copy-only'
 OR current_database() NOT LIKE 'af_partition_acceptance_%' THEN RAISE EXCEPTION 'explicit isolated restored copy required'; END IF;
 IF (SELECT count(*) FROM partition_ops.families)<>22 OR EXISTS(SELECT 1 FROM partition_ops.families WHERE phase<>'active') THEN RAISE EXCEPTION 'all append families must be active'; END IF;
 FOR f IN SELECT * FROM partition_ops.families ORDER BY family LOOP
  IF NOT EXISTS(SELECT 1 FROM pg_partitioned_table WHERE partrelid=f.parent_oid AND partstrat='r') THEN RAISE EXCEPTION 'range parent missing %',f.family; END IF;
  IF NOT EXISTS(SELECT 1 FROM pg_index x JOIN pg_class c ON c.oid=x.indexrelid JOIN pg_am a ON a.oid=c.relam WHERE x.indrelid=f.parent_oid AND a.amname='brin' AND x.indisvalid) THEN RAISE EXCEPTION 'BRIN missing %',f.family; END IF;
  FOR i IN SELECT key,value FROM jsonb_each(f.indexes) LOOP
   original_tail:=substring(i.value->>'definition' FROM ' USING .*$');
   SELECT substring(pg_get_indexdef(x.indexrelid) FROM ' USING .*$') INTO copied_tail FROM pg_index x JOIN pg_class c ON c.oid=x.indexrelid WHERE x.indrelid=f.registry_oid AND c.relname=i.value->>'registry_name' AND x.indisunique AND x.indisvalid;
   IF copied_tail IS DISTINCT FROM original_tail THEN RAISE EXCEPTION 'global key semantics changed: %.%',f.family,i.key; END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM pg_class WHERE oid=f.source_oid AND relrowsecurity IS DISTINCT FROM (SELECT relrowsecurity FROM pg_class WHERE oid=f.parent_oid)) OR EXISTS(SELECT 1 FROM pg_class WHERE oid=f.source_oid AND relforcerowsecurity IS DISTINCT FROM (SELECT relforcerowsecurity FROM pg_class WHERE oid=f.parent_oid)) THEN RAISE EXCEPTION 'RLS mode changed %',f.family; END IF;
  IF EXISTS(SELECT 1 FROM pg_constraint WHERE contype='f' AND confrelid=f.source_oid AND conparentid=0) THEN RAISE EXCEPTION 'incoming FK still targets only legacy heap %',f.family; END IF;
 END LOOP;
 IF EXISTS(SELECT 1 FROM partition_ops.foreign_keys WHERE phase<>'rebound') THEN RAISE EXCEPTION 'FK phases incomplete'; END IF;
END $checks$;
-- Run separately: real concurrent replay, FK RESTRICT/CASCADE/SET NULL and exact
-- composite tenant/user/session negatives; seal versus active lease/call UPDATE,
-- interrupted DETACH (inhdetachpending -> FINALIZE), archive corruption and
-- reattach-before-drop, restore-after-drop. Catalog equality is not those tests.
