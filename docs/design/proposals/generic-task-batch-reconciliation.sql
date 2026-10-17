-- INACTIVE UNNUMBERED PROPOSAL. Native authority assigns an epoch, role grants,
-- canonical fingerprints and actual-role acceptance. Never source directly.
-- Existing task_deliveries/epoch020 and all frozen history remain unchanged.
-- Resource grants come from the credential-owning upload authority, never caller JSON.
CREATE TABLE gateway.task_batch_inputs (
    tenant_id varchar(255) NOT NULL, user_id varchar(255) NOT NULL,
    service_id varchar(255) NOT NULL, provider_id varchar(255) NOT NULL,
    model_id varchar(255) NOT NULL, input_file_id varchar(255) NOT NULL, endpoint varchar(255) NOT NULL,
    input_sha256 char(64) NOT NULL CHECK(input_sha256 ~ '^[0-9a-f]{64}$'),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY(tenant_id,user_id,service_id,provider_id,input_file_id),
    CHECK(endpoint IN ('/v1/responses','/v1/chat/completions','/v1/embeddings'))
);
CREATE TABLE gateway.task_provider_batches (
    task_id varchar(255) PRIMARY KEY REFERENCES gateway.task_deliveries(task_id),
    request_id varchar(255) NOT NULL, service_id varchar(255) NOT NULL,
    user_id varchar(255) NOT NULL, tenant_id varchar(255) NOT NULL,
    claim_id uuid UNIQUE NOT NULL, provider_id varchar(255) NOT NULL,
    model_id varchar(255) NOT NULL, input_file_id varchar(255) NOT NULL, endpoint varchar(255) NOT NULL,
    input_sha256 char(64) NOT NULL CHECK(input_sha256 ~ '^[0-9a-f]{64}$'),
    submission_nonce uuid UNIQUE NOT NULL,
    provider_job_id varchar(255),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    confirmed_at timestamptz,
    CHECK((provider_job_id IS NULL)=(confirmed_at IS NULL)),
    FOREIGN KEY(tenant_id,user_id,service_id,provider_id,input_file_id)
      REFERENCES gateway.task_batch_inputs(tenant_id,user_id,service_id,provider_id,input_file_id)
);
CREATE TABLE gateway.task_reconciliation_audit (
    task_id varchar(255) NOT NULL REFERENCES gateway.task_deliveries(task_id),
    claim_id uuid NOT NULL,
    tenant_id varchar(255) NOT NULL, operator_user_id varchar(255) NOT NULL,
    provider_job_id varchar(255) NOT NULL,
    receipt_sha256 char(64) NOT NULL CHECK(receipt_sha256 ~ '^[0-9a-f]{64}$'),
    outcome varchar(32) NOT NULL CHECK(outcome IN ('completed','failed')),
    observed_at timestamptz NOT NULL, resolved_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(task_id,claim_id)
);
REVOKE ALL ON gateway.task_batch_inputs,gateway.task_provider_batches,gateway.task_reconciliation_audit FROM PUBLIC;
-- Native authority must assign exact current role names and fingerprints:
-- Gateway: batch_inputs SELECT only; provider_batches SELECT/INSERT plus column
-- UPDATE(provider_job_id,confirmed_at); reconciliation_audit SELECT/INSERT only.
-- Upload-grant INSERT belongs to the separately admitted credential/upload authority;
-- no user endpoint may grant arbitrary provider file IDs. No DELETE/TRUNCATE/CREATE.
-- Runtime/Worker/Knowledge/public: no access to any of these three tables.

-- Database intent binding is authoritative even for broader Gateway projection writers.
CREATE FUNCTION gateway.task_batch_binding() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $binding$
BEGIN
 IF TG_OP='UPDATE' THEN
  IF (to_jsonb(NEW)-ARRAY['provider_job_id','confirmed_at']) IS DISTINCT FROM (to_jsonb(OLD)-ARRAY['provider_job_id','confirmed_at'])
     OR (OLD.provider_job_id IS NOT NULL AND NEW.provider_job_id IS DISTINCT FROM OLD.provider_job_id)
     OR NEW.provider_job_id IS NULL THEN RAISE EXCEPTION 'immutable batch intent'; END IF;
  RETURN NEW;
 END IF;
 IF NOT EXISTS(SELECT 1 FROM gateway.task_deliveries d
  JOIN gateway.task_batch_inputs i ON i.tenant_id=d.tenant_id AND i.user_id=d.user_id AND i.service_id=d.service_id
  WHERE d.task_id=NEW.task_id AND d.request_id=NEW.request_id AND d.service_id=NEW.service_id
    AND d.user_id=NEW.user_id AND d.tenant_id=NEW.tenant_id AND d.claim_id=NEW.claim_id AND d.state='claimed_unknown'
    AND d.payload->'envelope'->'request'->'parameters'->'provider_batch'->>'provider_id'=NEW.provider_id
    AND d.payload->'envelope'->'request'->'parameters'->'provider_batch'->>'model_id'=NEW.model_id
    AND d.payload->'envelope'->'request'->'parameters'->'provider_batch'->>'endpoint'=NEW.endpoint
    AND d.payload->'envelope'->'request'->'parameters'->'provider_batch'->>'input_file_id'=NEW.input_file_id
    AND i.provider_id=NEW.provider_id AND i.model_id=NEW.model_id AND i.input_file_id=NEW.input_file_id
    AND i.endpoint=NEW.endpoint AND i.input_sha256=NEW.input_sha256 AND i.expires_at>clock_timestamp())
 THEN RAISE EXCEPTION 'batch intent scope or claim mismatch'; END IF;
 RETURN NEW;
END
$binding$;
CREATE TRIGGER task_batch_bound BEFORE INSERT OR UPDATE ON gateway.task_provider_batches FOR EACH ROW EXECUTE FUNCTION gateway.task_batch_binding();
REVOKE ALL ON FUNCTION gateway.task_batch_binding() FROM PUBLIC;

-- Expansion/contraction must be measured by native authority; this proposal is
-- based on the exact post-028 constraint, never changes any frozen source.
ALTER TABLE gateway.price_book_v2_usage_receipts ADD CONSTRAINT price_book_v2_batch_receipt_check CHECK(
 (snapshot->>'tenant_id'=tenant_id AND snapshot->>'provider_id'=provider_id AND snapshot->>'model_id'=model_id AND snapshot->>'currency'='USD'
 AND (snapshot->>'unit'='per_1k_tokens' OR (snapshot->>'unit'='per_request' AND snapshot->>'pricing_meter'='requests' AND snapshot->'price_book_request'->'entry'->'key'->>'component'='request' AND snapshot->'price_book_request'->'entry'->>'unit'='requests') OR
 (request_type='provider_batch' AND snapshot->>'unit'='per_batch_request' AND snapshot->>'pricing_meter'='provider_batch'
 AND snapshot->'batch_receipt'->'entry'->'key'->>'component'='batch'
 AND snapshot->'batch_receipt'->'entry'->>'unit'='requests'))) IS TRUE) NOT VALID;
-- The authority must validate expansion before dropping the old narrower check.
ALTER TABLE gateway.price_book_v2_usage_receipts VALIDATE CONSTRAINT price_book_v2_batch_receipt_check;
ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_request_v2_check;
ALTER TABLE gateway.price_book_v2_accounting_checkpoints ADD CONSTRAINT price_book_v2_batch_stage_check CHECK(stage IN ('quota_counters','daily_aggregates','hourly_aggregates','batch_receipt_verified')) NOT VALID;
ALTER TABLE gateway.price_book_v2_accounting_checkpoints VALIDATE CONSTRAINT price_book_v2_batch_stage_check;
ALTER TABLE gateway.price_book_v2_accounting_checkpoints DROP CONSTRAINT price_book_v2_accounting_checkpoints_stage_check;
-- This dedicated checkpoint records verified cost, not fabricated quota/daily/hourly application.
-- Existing checkpoint->immutable receipt trigger remains in force without alteration.
CREATE TABLE gateway.task_batch_contract(version integer PRIMARY KEY CHECK(version=1));
REVOKE ALL ON gateway.task_batch_contract FROM PUBLIC;
-- Grant Gateway SELECT only. Insert marker only after schema, grants and price
-- contract acceptance; tables alone never activate submission. Others no access.
-- INSERT INTO gateway.task_batch_contract VALUES(1); -- operator activation, NOT automatic
-- Exact role grants are part of this proposal, never application startup DDL.
DO $grants$
DECLARE principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN RAISE EXCEPTION 'canonical owner required'; END IF;
 principal:=left(current_user,length(current_user)-length('owner'))||'gateway';
 EXECUTE format('GRANT SELECT ON gateway.task_batch_inputs,gateway.task_batch_contract TO %I',principal);
 EXECUTE format('GRANT SELECT,INSERT ON gateway.task_provider_batches,gateway.task_reconciliation_audit TO %I',principal);
 EXECUTE format('GRANT UPDATE(provider_job_id,confirmed_at) ON gateway.task_provider_batches TO %I',principal);
END
$grants$;

-- Input count is measured by the credential-owning upload grant authority.
ALTER TABLE gateway.task_batch_inputs ADD COLUMN request_count bigint NOT NULL CHECK(request_count BETWEEN 1 AND 50000);
ALTER TABLE gateway.task_provider_batches ADD CONSTRAINT task_provider_batches_task_claim_unique UNIQUE(task_id,claim_id);
CREATE TABLE gateway.task_batch_quota_holds(
 task_id varchar(255) PRIMARY KEY,
 claim_id uuid UNIQUE NOT NULL,
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 request_count bigint NOT NULL CHECK(request_count BETWEEN 1 AND 50000),
 reserved_cost_cents bigint NOT NULL CHECK(reserved_cost_cents>=0),
 state text NOT NULL DEFAULT 'held' CHECK(state IN ('held','settled')),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),settled_at timestamptz,
 CHECK((state='settled')=(settled_at IS NOT NULL)),
 FOREIGN KEY(task_id,claim_id) REFERENCES gateway.task_provider_batches(task_id,claim_id)
);
CREATE INDEX task_batch_quota_pending ON gateway.task_batch_quota_holds(tenant_id,user_id) WHERE state='held';
CREATE FUNCTION gateway.task_batch_hold_binding() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $hold$
BEGIN
 IF TG_OP='UPDATE' THEN
  IF (to_jsonb(NEW)-ARRAY['state','settled_at']) IS DISTINCT FROM (to_jsonb(OLD)-ARRAY['state','settled_at'])
    OR OLD.state<>'held' OR NEW.state<>'settled' THEN RAISE EXCEPTION 'immutable batch hold'; END IF;
 ELSE
  IF NEW.state<>'held' OR NOT EXISTS(SELECT 1 FROM gateway.task_provider_batches b JOIN gateway.task_batch_inputs i
   ON i.tenant_id=b.tenant_id AND i.user_id=b.user_id AND i.service_id=b.service_id AND i.provider_id=b.provider_id AND i.input_file_id=b.input_file_id
   WHERE b.task_id=NEW.task_id AND b.claim_id=NEW.claim_id AND b.tenant_id=NEW.tenant_id AND b.user_id=NEW.user_id
   AND i.model_id=b.model_id AND i.input_sha256=b.input_sha256 AND i.request_count=NEW.request_count)
  THEN RAISE EXCEPTION 'batch hold scope mismatch'; END IF;
 END IF;
 RETURN NEW;
END
$hold$;
CREATE TRIGGER task_batch_hold_bound BEFORE INSERT OR UPDATE ON gateway.task_batch_quota_holds FOR EACH ROW EXECUTE FUNCTION gateway.task_batch_hold_binding();
REVOKE ALL ON gateway.task_batch_quota_holds FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.task_batch_hold_binding() FROM PUBLIC;
DO $hold_grants$
DECLARE principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN RAISE EXCEPTION 'canonical owner required'; END IF;
 principal:=left(current_user,length(current_user)-length('owner'))||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.task_batch_quota_holds TO %I',principal);
 EXECUTE format('GRANT UPDATE(state,settled_at) ON gateway.task_batch_quota_holds TO %I',principal);
END
$hold_grants$;
ALTER TABLE gateway.dispatch_token_reservations ADD CONSTRAINT dispatch_token_reservations_batch_type_check CHECK(request_type IN ('invoke','stream','provider_batch')) NOT VALID;
ALTER TABLE gateway.dispatch_token_reservations VALIDATE CONSTRAINT dispatch_token_reservations_batch_type_check;
ALTER TABLE gateway.dispatch_token_reservations DROP CONSTRAINT dispatch_token_reservations_request_type_check;
-- Preserve the exact token/request branches, admitting Batch only with its own frozen meter.
ALTER TABLE gateway.usage_records ADD CONSTRAINT usage_records_batch_meter_v1 CHECK((
 (meter_kind='tokens' AND input_tokens IS NOT NULL AND output_tokens IS NOT NULL)
 OR (meter_kind='requests' AND input_tokens IS NULL AND output_tokens IS NULL AND measured_units->>'tokens'='not_applicable'
 AND measured_units->>'requests' ~ '^[0-9]+$' AND (measured_units->>'requests')::numeric<=9223372036854775807
 AND (metadata->'pricing_snapshot'->>'pricing_meter'='requests'
 OR (request_type='provider_batch' AND metadata->'pricing_snapshot'->>'pricing_meter'='provider_batch'))
 AND request_cost_nanos IS NOT NULL AND request_cost_nanos>=0)) IS TRUE) NOT VALID;
ALTER TABLE gateway.usage_records VALIDATE CONSTRAINT usage_records_batch_meter_v1;
ALTER TABLE gateway.usage_records DROP CONSTRAINT usage_records_meter_v1;
