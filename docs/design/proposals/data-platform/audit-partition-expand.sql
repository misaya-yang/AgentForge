-- INERT AUTHORITY EXPAND PROPOSAL; no baseline/epoch/fingerprint edits.
-- First supported table is append-only gateway.audit_logs, with unchanged global ID uniqueness.
-- Do not apply this design mechanically to usage_records: its global request identity
-- unique index and nullable created_at require a separate business-compatible successor.
SET LOCAL lock_timeout = '2s';
SET LOCAL statement_timeout = '60s';
DO $preflight$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE contype='f'
             AND (confrelid='gateway.audit_logs'::regclass OR conrelid='gateway.audit_logs'::regclass)) THEN
    RAISE EXCEPTION 'audit partitioning requires explicit FK compatibility design';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_index WHERE indrelid='gateway.audit_logs'::regclass AND indisunique AND indexrelid<>'gateway.audit_logs_pkey'::regclass) THEN
    RAISE EXCEPTION 'additional global uniqueness requires its own partition design'; END IF;
  IF EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid='gateway.audit_logs'::regclass AND NOT tgisinternal) THEN
    RAISE EXCEPTION 'existing audit triggers require explicit parent compatibility design'; END IF;
END $preflight$;
CREATE TABLE gateway.audit_log_identity (
  id bigint PRIMARY KEY,
  created_at timestamptz NOT NULL,
  UNIQUE (id,created_at)
);
ALTER TABLE gateway.audit_log_identity OWNER TO ai_gateway_owner;
REVOKE ALL ON gateway.audit_log_identity FROM PUBLIC,ai_gateway_gateway,
  ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker;
CREATE FUNCTION gateway.claim_audit_log_identity() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $body$
BEGIN
  IF TG_OP<>'INSERT' OR TG_WHEN<>'BEFORE' OR TG_LEVEL<>'ROW' OR TG_NARGS<>0
     OR TG_TABLE_SCHEMA<>'gateway' THEN RAISE EXCEPTION 'invalid audit identity trigger'; END IF;
  -- Preserve the old PRIMARY KEY(id) behavior across all physical months.
  INSERT INTO gateway.audit_log_identity(id,created_at) VALUES(NEW.id,NEW.created_at);
  RETURN NEW;
END $body$;
ALTER FUNCTION gateway.claim_audit_log_identity() OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION gateway.claim_audit_log_identity() FROM PUBLIC;
CREATE TRIGGER audit_global_identity BEFORE INSERT ON gateway.audit_logs
  FOR EACH ROW EXECUTE FUNCTION gateway.claim_audit_log_identity();

CREATE TABLE gateway.audit_logs_partitioned (
  LIKE gateway.audit_logs INCLUDING DEFAULTS INCLUDING GENERATED INCLUDING CONSTRAINTS INCLUDING STORAGE INCLUDING COMMENTS,
  PRIMARY KEY(id,created_at),
  FOREIGN KEY(id,created_at) REFERENCES gateway.audit_log_identity(id,created_at)
) PARTITION BY RANGE(created_at);
ALTER TABLE gateway.audit_logs_partitioned OWNER TO ai_gateway_owner;
REVOKE ALL ON gateway.audit_logs_partitioned FROM PUBLIC,ai_gateway_gateway,
  ai_gateway_runtime,ai_gateway_capability_worker,ai_gateway_knowledge_api,ai_gateway_knowledge_worker;
CREATE INDEX audit_logs_partitioned_created_brin ON gateway.audit_logs_partitioned USING brin(created_at);
CREATE INDEX audit_logs_partitioned_tenant_time ON gateway.audit_logs_partitioned(tenant_id,created_at DESC,id DESC);
CREATE INDEX audit_logs_partitioned_user_time ON gateway.audit_logs_partitioned(user_id,created_at DESC);
CREATE INDEX audit_logs_partitioned_event_type ON gateway.audit_logs_partitioned(event_type);
CREATE INDEX audit_logs_partitioned_agent_dimensions ON gateway.audit_logs_partitioned
  (tenant_id,agent_id,agent_version_id,publication_id,channel,created_at DESC,id DESC)
  WHERE agent_id IS NOT NULL;

CREATE FUNCTION gateway.backfill_audit_identity(p_after bigint,p_limit integer DEFAULT 10000)
RETURNS TABLE(last_id bigint,rows_copied bigint) LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $body$
BEGIN
  IF p_limit<1 OR p_limit>10000 THEN RAISE EXCEPTION 'backfill batch outside bounds'; END IF;
  RETURN QUERY WITH batch AS MATERIALIZED (
    SELECT id,created_at FROM gateway.audit_logs WHERE p_after IS NULL OR id>p_after ORDER BY id LIMIT p_limit
  ), copied AS (
    INSERT INTO gateway.audit_log_identity(id,created_at) SELECT id,created_at FROM batch
    ON CONFLICT(id) DO NOTHING RETURNING id
  ) SELECT max(batch.id), (SELECT count(*) FROM copied) FROM batch;
END $body$;
ALTER FUNCTION gateway.backfill_audit_identity(bigint,integer) OWNER TO ai_gateway_owner;
REVOKE ALL ON FUNCTION gateway.backfill_audit_identity(bigint,integer) FROM PUBLIC;
ALTER TABLE gateway.audit_logs ADD CONSTRAINT audit_logs_partition_ready_identity_fk
  FOREIGN KEY(id,created_at) REFERENCES gateway.audit_log_identity(id,created_at) NOT VALID;
-- Authority runs bounded committed batches as owner, recording last_id; application roles get no EXECUTE.
