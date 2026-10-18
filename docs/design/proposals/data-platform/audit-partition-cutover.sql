-- INERT AUTHORITY CONTRACT PHASE. Requires approved immutable cutover month input,
-- completed identity backfill, valid concurrent index, dependency review and restored-copy rehearsal.
-- Rollback class: restore-required once retention drops a detached partition.
SET LOCAL lock_timeout = '2s';
SET LOCAL statement_timeout = '5min';
SET LOCAL TIME ZONE 'UTC';
LOCK TABLE gateway.audit_logs IN ACCESS EXCLUSIVE MODE;
DO $cutover$
DECLARE boundary timestamptz := current_setting('agentforge.audit_partition_cutover')::timestamptz;
BEGIN
  IF boundary<>date_trunc('month',boundary) THEN RAISE EXCEPTION 'UTC month boundary required'; END IF;
  IF EXISTS (SELECT 1 FROM gateway.audit_logs WHERE created_at>=boundary) THEN
    RAISE EXCEPTION 'legacy rows overlap new monthly partitions'; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.audit_logs'::regclass AND conname='audit_logs_partition_ready_identity_fk' AND convalidated) THEN
    RAISE EXCEPTION 'validated global identity foreign key required'; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index WHERE
     indexrelid='gateway.audit_logs_partition_ready_id_time'::regclass AND indisvalid AND indisunique) THEN
    RAISE EXCEPTION 'valid online unique index required'; END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.audit_logs'::regclass AND conname='audit_logs_legacy_bound' AND convalidated) THEN
    RAISE EXCEPTION 'legacy month bound must be validated online before cutover'; END IF;
  -- The global registry + validated FK now preserves id uniqueness independently.
  -- Remove the legacy local primary key before the parent composite PK is attached.
  ALTER TABLE gateway.audit_logs DROP CONSTRAINT audit_logs_pkey;
  ALTER TABLE gateway.audit_logs RENAME TO audit_logs_legacy;
  ALTER TABLE gateway.audit_logs_partitioned RENAME TO audit_logs;
  EXECUTE format('ALTER TABLE gateway.audit_logs ATTACH PARTITION gateway.audit_logs_legacy FOR VALUES FROM (MINVALUE) TO (%L)',boundary);
END $cutover$;
-- Move sequence ownership before a future legacy-partition drop could otherwise cascade it.
ALTER SEQUENCE gateway.audit_logs_id_seq OWNED BY gateway.audit_logs.id;
-- Remove the online-expansion leaf trigger before parent trigger cloning.
DROP TRIGGER audit_global_identity ON gateway.audit_logs_legacy;
CREATE TRIGGER audit_global_identity BEFORE INSERT ON gateway.audit_logs
  FOR EACH ROW EXECUTE FUNCTION gateway.claim_audit_log_identity();
GRANT SELECT,INSERT ON gateway.audit_logs TO ai_gateway_gateway,ai_gateway_knowledge_api,ai_gateway_knowledge_worker;
-- Existing sequence USAGE grants stay unchanged. No DELETE/TRUNCATE/app DDL grants.
CREATE FUNCTION gateway.prepare_audit_months(p_first timestamptz,p_count integer)
RETURNS void LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp SET TimeZone='UTC' AS $body$
DECLARE n integer; lo timestamptz; hi timestamptz; partition_name text;
BEGIN
  IF p_count<1 OR p_count>24 OR p_first<>date_trunc('month',p_first,'UTC') THEN
    RAISE EXCEPTION 'bounded UTC monthly preparation required'; END IF;
  FOR n IN 0..p_count-1 LOOP
    lo:=p_first+(n*interval '1 month'); hi:=lo+interval '1 month';
    partition_name:='audit_logs_p'||to_char(lo AT TIME ZONE 'UTC','YYYYMM');
    IF to_regclass('gateway.'||partition_name) IS NULL THEN
      EXECUTE format('CREATE TABLE gateway.%I PARTITION OF gateway.audit_logs FOR VALUES FROM (%L) TO (%L)',partition_name,lo,hi);
    ELSE
      -- Existing objects require authority bound/owner/index verification, never blind reuse.
      RAISE EXCEPTION 'partition already exists; verify its bounds through authority';
    END IF;
  END LOOP;
END $body$;
ALTER FUNCTION gateway.prepare_audit_months(timestamptz,integer) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION gateway.prepare_audit_months(timestamptz,integer) FROM PUBLIC;
SELECT gateway.prepare_audit_months(current_setting('agentforge.audit_partition_cutover')::timestamptz,4);
