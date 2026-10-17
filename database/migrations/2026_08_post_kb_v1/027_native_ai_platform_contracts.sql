-- UNACTIVATED, unnumbered fixed source bundle for canonical successor 027 v2.
-- Expansion-only: retain both original 026 price-book CHECKs; the exact two drops belong to 028.
-- Generated from five exact proposal inputs in the recorded dependency order.
-- Two source-level BEGIN/COMMIT wrappers are removed before the single native transaction.
-- No distribution pin, numbered migration, database action, or activation is included.

-- Native-authority owner boundary for every input in this fixed bundle.
DO $successor_027_owner$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
    OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
    OR (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)=current_user) IS NOT TRUE
    OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))
 THEN RAISE EXCEPTION 'canonical unprivileged schema owner required'; END IF;
END
$successor_027_owner$;

-- BEGIN FIXED SOURCE request-meter-v1: database/proposals/request-meter-v1.sql
-- Extends 026 price-book and usage-receipt checks with request metering and typed N/A token facts.
-- Frozen source SHA-256: 3a2ab7661a2ce25f00118c894746d8da58d084695eae62316d0f467a29788ae6
-- PROPOSAL ONLY. Independent successor to measured price-book/schema 026.
-- Apply before enabling registered request-meter models; never modify 026.
DO $owner$
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR current_user<>(SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname='gateway') THEN RAISE EXCEPTION 'canonical Gateway owner required'; END IF;
END $owner$;
-- Preserve every original price component wire value; append request explicitly.
-- Retain the 026 component CHECK; contract it only in the reviewed 028 successor.
ALTER TABLE gateway.price_book_v2_terms ADD CONSTRAINT price_book_v2_terms_component_request_v2_check CHECK(component IN ('input','cached_input','cache_write','output','reasoning','tool_call','image','batch','request')) NOT VALID;
ALTER TABLE gateway.price_book_v2_terms VALIDATE CONSTRAINT price_book_v2_terms_component_request_v2_check;
-- Retain the 026 receipt CHECK; contract it only in the reviewed 028 successor.
ALTER TABLE gateway.price_book_v2_usage_receipts ADD CONSTRAINT price_book_v2_usage_receipts_request_v2_check CHECK((snapshot->>'tenant_id'=tenant_id AND snapshot->>'provider_id'=provider_id AND snapshot->>'model_id'=model_id AND snapshot->>'currency'='USD' AND (snapshot->>'unit'='per_1k_tokens' OR (snapshot->>'unit'='per_request' AND snapshot->>'pricing_meter'='requests' AND snapshot->'price_book_request'->'entry'->'key'->>'component'='request' AND snapshot->'price_book_request'->'entry'->>'unit'='requests'))) IS TRUE) NOT VALID;
ALTER TABLE gateway.price_book_v2_usage_receipts VALIDATE CONSTRAINT price_book_v2_usage_receipts_request_v2_check;
-- Physical token facts are NULL for N/A, not invented observations of zero.
-- Legacy INSERTs keep their existing defaults and constraints; no caller must
-- change a token row. Price-book raw receipt checks still forbid meter mutation.
ALTER TABLE gateway.usage_records ALTER COLUMN input_tokens DROP NOT NULL;
ALTER TABLE gateway.usage_records ALTER COLUMN output_tokens DROP NOT NULL;
ALTER TABLE gateway.usage_records ADD COLUMN meter_kind text GENERATED ALWAYS AS(CASE WHEN metadata->'measured_units'->>'meter'='requests' THEN 'requests' ELSE 'tokens' END) STORED;
ALTER TABLE gateway.usage_records ADD COLUMN measured_units jsonb GENERATED ALWAYS AS(metadata->'measured_units') STORED;
ALTER TABLE gateway.usage_records ADD COLUMN request_cost_nanos bigint GENERATED ALWAYS AS(CASE WHEN metadata->'measured_units'->>'meter'='requests' THEN (metadata->>'request_cost_nanos')::bigint ELSE NULL END) STORED;
ALTER TABLE gateway.usage_records ADD COLUMN token_count_state text GENERATED ALWAYS AS(CASE WHEN metadata->'measured_units'->>'meter'='requests' THEN 'not_applicable' WHEN input_tokens IS NULL OR output_tokens IS NULL THEN 'unknown' ELSE 'reported' END) STORED;
ALTER TABLE gateway.usage_records ADD CONSTRAINT usage_records_meter_v1 CHECK((
 (meter_kind='tokens' AND input_tokens IS NOT NULL AND output_tokens IS NOT NULL)
 OR (meter_kind='requests' AND input_tokens IS NULL AND output_tokens IS NULL AND measured_units->>'tokens'='not_applicable' AND measured_units->>'requests' ~ '^[0-9]+$' AND (measured_units->>'requests')::numeric<=9223372036854775807 AND metadata->'pricing_snapshot'->>'pricing_meter'='requests' AND request_cost_nanos IS NOT NULL AND request_cost_nanos>=0)
) IS TRUE) NOT VALID;
ALTER TABLE gateway.usage_records VALIDATE CONSTRAINT usage_records_meter_v1;
-- No new writable table or broad privilege. Generated columns require no INSERT
-- grants. Existing authorized Gateway ledger readers retain aggregate cost and
-- invocation counts; request physical token values are nullable typed facts.
-- An ordinary legacy SUM(tokens) remains the sum of applicable known token facts.
-- Billing summary adds metering annotation when request facts exist; cache ratios
-- exclude N/A request rows instead of decoding them as zero or cache unknown.
-- END FIXED SOURCE request-meter-v1

-- BEGIN FIXED SOURCE knowledge-model-plane: database/proposals/knowledge-model-plane.sql
-- Adds Gateway-owned, quota-locked model-call reservations and idempotent settlement on request-meter facts.
-- Frozen source SHA-256: ff5900f75ba8ebb93b20926736651a5c99abd50a57fff7cb0236efcec1cc5ad2
-- PROPOSAL ONLY. Unnumbered: native authority must admit/measure and grant roles.
-- Execute as migration owner, never Knowledge or runtime. No existing migration edited.
CREATE TABLE gateway.knowledge_model_calls (
 lease_id uuid PRIMARY KEY,
 tenant_id varchar(255) NOT NULL, user_id varchar(255) NOT NULL,
 call_id varchar(128) NOT NULL, request_sha256 char(64) NOT NULL,
 lease jsonb NOT NULL, call jsonb NOT NULL, snapshot jsonb NOT NULL, pricing jsonb NOT NULL,
 state text NOT NULL DEFAULT 'reserved' CHECK(state IN ('reserved','dispatched','observed','unknown','completed','rejected')),
 reserved_input bigint NOT NULL CHECK(reserved_input>=0), reserved_output bigint NOT NULL CHECK(reserved_output>=0),
 reserved_cost_microcents bigint NOT NULL CHECK(reserved_cost_microcents>=0),
 reserved_cost_nanos numeric(38,0) NOT NULL CHECK(reserved_cost_nanos>=0),
 quota_daily_period timestamptz, quota_monthly_period timestamptz,
 issued_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL,
 dispatched_at timestamptz, completed_at timestamptz, provider_request_id text,
 error_code text, observation jsonb, result jsonb,
 UNIQUE(tenant_id,user_id,call_id)
);
CREATE TABLE gateway.knowledge_model_workload_nonces (
 key_id text NOT NULL, nonce text NOT NULL, expires_at timestamptz NOT NULL,
 PRIMARY KEY(key_id,nonce)
);
CREATE FUNCTION gateway.reserve_knowledge_model_call(
 p_tenant text,p_user text,p_call text,p_hash text,p_lease uuid,p_token jsonb,p_body jsonb,p_snapshot jsonb,p_price jsonb,p_input bigint,p_output bigint,p_cost bigint
) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway AS $$
DECLARE existing gateway.knowledge_model_calls; expired gateway.knowledge_model_calls; q gateway.user_quotas; tokens bigint; cents bigint; refund bigint; cost_nanos numeric; spent_nanos numeric; pending_nanos numeric; period_start timestamptz;
BEGIN
 IF p_input<0 OR p_output<0 OR p_cost<0 OR p_hash !~ '^[0-9a-f]{64}$' OR p_token->>'tenant_id'<>p_tenant OR p_token->>'user_id'<>p_user OR p_token->>'call_id'<>p_call OR p_token->>'request_sha256'<>p_hash OR p_body->'scope'->'actor'->>'tenant_id'<>p_tenant OR p_body->'scope'->'actor'->>'user_id'<>p_user THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_INVALID'; END IF;
 -- Per-actor quota row serializes reservations AND duplicate first creation.
 SELECT * INTO q FROM gateway.user_quotas WHERE tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_QUOTA_NOT_PROVISIONED'; END IF;
 SELECT * INTO existing FROM gateway.knowledge_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND call_id=p_call;
 IF FOUND THEN
  IF existing.request_sha256<>p_hash OR existing.call<>p_body THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_BODY_CONFLICT'; END IF;
  RETURN to_jsonb(existing);
 END IF;
 -- Only expired never-dispatched reservations can be safely released. The
 -- quota lock and reserved-state row lock serialize this against dispatch CAS.
 FOR expired IN SELECT * FROM gateway.knowledge_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND state='reserved' AND expires_at<=NOW() FOR UPDATE LOOP
  refund:=(expired.reserved_cost_microcents/10000)+CASE WHEN expired.reserved_cost_microcents%10000>0 THEN 1 ELSE 0 END;
  UPDATE gateway.user_quotas SET
   current_daily_tokens=GREATEST(0,COALESCE(current_daily_tokens,0)-CASE WHEN daily_reset_at IS NOT DISTINCT FROM expired.quota_daily_period THEN expired.reserved_input+expired.reserved_output ELSE 0 END),
   current_monthly_tokens=GREATEST(0,COALESCE(current_monthly_tokens,0)-CASE WHEN monthly_reset_at IS NOT DISTINCT FROM expired.quota_monthly_period THEN expired.reserved_input+expired.reserved_output ELSE 0 END),
   current_monthly_cost_cents=GREATEST(0,COALESCE(current_monthly_cost_cents,0)-CASE WHEN monthly_reset_at IS NOT DISTINCT FROM expired.quota_monthly_period THEN refund ELSE 0 END),
   current_daily_requests=GREATEST(0,COALESCE(current_daily_requests,0)-CASE WHEN daily_reset_at IS NOT DISTINCT FROM expired.quota_daily_period THEN 1 ELSE 0 END)
  WHERE id=q.id;
  UPDATE gateway.knowledge_model_calls SET state='rejected',error_code='reservation_expired',completed_at=NOW(),
   result=jsonb_build_object('call_id',expired.call_id,'lease_id',expired.lease_id,'request_sha256',expired.request_sha256,'state','rejected','upstream_status',410,'output',NULL,
    'receipt',jsonb_build_object('receipt_id',expired.lease_id,'call_id',expired.call_id,'lease_id',expired.lease_id,'tenant_id',p_tenant,'user_id',p_user,'provider_id',expired.lease->'model'->>'provider','model',expired.lease->'model','price_book_revision',COALESCE(expired.pricing->'price_book_input'->'entry'->'key'->>'book_version',expired.pricing->'price_book_request'->'entry'->'key'->>'book_version'),'units',CASE WHEN expired.pricing->>'pricing_meter'='requests' THEN jsonb_build_object('requests',0) ELSE jsonb_build_object('input_tokens',0,'output_tokens',0) END,'settled',true,'metering',CASE WHEN expired.pricing->>'pricing_meter'='requests' THEN jsonb_build_object('kind','requests','tokens','not_applicable','cost_nanos',0) ELSE NULL END))
  WHERE lease_id=expired.lease_id AND state='reserved';
 END LOOP;
 SELECT * INTO q FROM gateway.user_quotas WHERE id=q.id;
 tokens:=p_input+p_output;
 -- Stored legacy cost fields use microcents: 1 cent = 10,000 legacy microcents.
 cents:=(p_cost/10000)+CASE WHEN p_cost%10000>0 THEN 1 ELSE 0 END;
 -- Keep the existing quota counter semantics, but do not let their legacy
 -- per-observation cent rounding turn repeated sub-cent requests into a quota
 -- bypass. Exact request facts and pending ceilings share this same quota lock.
 cost_nanos:=CASE WHEN p_price->>'pricing_meter'='requests' THEN ceil((p_price->'price_book_request'->'entry'->>'rate_nanos')::numeric*(10000-COALESCE((p_price->'price_book_request'->'discount'->>'discount_bps')::numeric,0))/(10000*(p_price->'price_book_request'->'entry'->>'units_per_rate')::numeric)) ELSE p_cost::numeric*1000 END;
 IF cost_nanos IS NULL OR cost_nanos<0 THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_INVALID_PRICE'; END IF;
 period_start:=CASE WHEN q.monthly_reset_at>NOW() THEN q.monthly_reset_at-interval '1 month' ELSE COALESCE(q.monthly_reset_at,date_trunc('month',NOW())) END;
 SELECT COALESCE(SUM(CASE WHEN metadata->'measured_units'->>'meter'='requests' THEN (metadata->>'request_cost_nanos')::numeric ELSE total_cost_cents::numeric*1000 END),0) INTO spent_nanos FROM gateway.usage_records WHERE tenant_id=p_tenant AND user_id=p_user AND created_at>=period_start;
 SELECT COALESCE(SUM(reserved_cost_nanos),0) INTO pending_nanos FROM gateway.knowledge_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND state IN ('reserved','dispatched','observed','unknown') AND quota_monthly_period IS NOT DISTINCT FROM q.monthly_reset_at;
 IF COALESCE(q.is_blocked,FALSE)
 OR (q.daily_token_limit IS NOT NULL AND COALESCE(q.current_daily_tokens,0)+tokens>q.daily_token_limit)
 OR (q.monthly_token_limit IS NOT NULL AND COALESCE(q.current_monthly_tokens,0)+tokens>q.monthly_token_limit)
 OR (q.monthly_cost_limit_cents IS NOT NULL AND COALESCE(q.current_monthly_cost_cents,0)+cents>q.monthly_cost_limit_cents)
 OR (q.monthly_cost_limit_cents IS NOT NULL AND spent_nanos+pending_nanos+cost_nanos>q.monthly_cost_limit_cents::numeric*10000000)
 OR (q.requests_per_day IS NOT NULL AND COALESCE(q.current_daily_requests,0)+1>q.requests_per_day)
 THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_QUOTA_EXHAUSTED'; END IF;
 UPDATE gateway.user_quotas SET current_daily_tokens=COALESCE(current_daily_tokens,0)+tokens,current_monthly_tokens=COALESCE(current_monthly_tokens,0)+tokens,current_monthly_cost_cents=COALESCE(current_monthly_cost_cents,0)+cents,current_daily_requests=COALESCE(current_daily_requests,0)+1 WHERE id=q.id;
 INSERT INTO gateway.knowledge_model_calls(lease_id,tenant_id,user_id,call_id,request_sha256,lease,call,snapshot,pricing,reserved_input,reserved_output,reserved_cost_microcents,reserved_cost_nanos,quota_daily_period,quota_monthly_period,expires_at)
 VALUES(p_lease,p_tenant,p_user,p_call,p_hash,p_token,p_body,p_snapshot,p_price,p_input,p_output,p_cost,cost_nanos,q.daily_reset_at,q.monthly_reset_at,to_timestamp((p_token->>'expires_at_unix')::double precision)) RETURNING * INTO existing;
 RETURN to_jsonb(existing);
END $$;
CREATE FUNCTION gateway.settle_knowledge_model_call(p_lease uuid,p_tenant text,p_user text,p_input bigint,p_output bigint,p_input_cost bigint,p_output_cost bigint,p_result jsonb,p_record jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway AS $$
DECLARE c gateway.knowledge_model_calls; q gateway.user_quotas; tokens bigint; reserved_cents bigint; actual_cents bigint;
BEGIN
 -- Same lock order as reserve. The call row is immutable in identity.
 SELECT * INTO q FROM gateway.user_quotas WHERE tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_QUOTA_NOT_PROVISIONED'; END IF;
 SELECT * INTO c FROM gateway.knowledge_model_calls WHERE lease_id=p_lease AND tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_NOT_FOUND'; END IF;
 IF c.result IS NOT NULL THEN RETURN c.result; END IF;
 IF c.state NOT IN ('observed','unknown') OR c.observation IS NULL OR p_input<0 OR p_output<0 OR p_input_cost<0 OR p_output_cost<0
 OR p_result->>'lease_id'<>p_lease::text OR p_result->>'request_sha256'<>c.request_sha256 OR p_result->'receipt'->>'tenant_id'<>p_tenant OR p_result->'receipt'->>'user_id'<>p_user OR p_result->'receipt'->>'settled'<>'true' THEN RAISE EXCEPTION 'KNOWLEDGE_MODEL_RECEIPT_INVALID'; END IF;
 tokens:=p_input+p_output;
 reserved_cents:=(c.reserved_cost_microcents/10000)+CASE WHEN c.reserved_cost_microcents%10000>0 THEN 1 ELSE 0 END;
 actual_cents:=((p_input_cost+p_output_cost)/10000)+CASE WHEN (p_input_cost+p_output_cost)%10000>0 THEN 1 ELSE 0 END;
 UPDATE gateway.user_quotas SET
 current_daily_tokens=GREATEST(0,COALESCE(current_daily_tokens,0)+CASE WHEN daily_reset_at IS NOT DISTINCT FROM c.quota_daily_period THEN -c.reserved_input-c.reserved_output ELSE 0 END),
 current_monthly_tokens=GREATEST(0,COALESCE(current_monthly_tokens,0)+CASE WHEN monthly_reset_at IS NOT DISTINCT FROM c.quota_monthly_period THEN -c.reserved_input-c.reserved_output ELSE 0 END),
 current_daily_requests=GREATEST(0,COALESCE(current_daily_requests,0)-CASE WHEN daily_reset_at IS NOT DISTINCT FROM c.quota_daily_period THEN 1 ELSE 0 END),
 current_monthly_cost_cents=GREATEST(0,COALESCE(current_monthly_cost_cents,0)+CASE WHEN monthly_reset_at IS NOT DISTINCT FROM c.quota_monthly_period THEN -reserved_cents ELSE 0 END)
 WHERE id=q.id;
 -- Raw ledger, immutable receipt and reservation release are in the caller transaction.
 -- The existing canonical quota/daily/hourly stages MUST run before its COMMIT.
 INSERT INTO gateway.usage_records(tenant_id,user_id,request_id,service_id,model,provider,input_tokens,output_tokens,input_cost_cents,output_cost_cents,status,request_type,metadata,created_at)
 VALUES(p_tenant,p_user,p_record->>'request_id','knowledge-model-plane',c.lease->'model'->>'model',c.lease->'model'->>'provider',CASE WHEN p_record->'metadata'->'measured_units'->>'meter'='requests' THEN NULL ELSE p_input END,CASE WHEN p_record->'metadata'->'measured_units'->>'meter'='requests' THEN NULL ELSE p_output END,p_input_cost,p_output_cost,
 CASE WHEN p_result->>'state'='completed' THEN 'success' ELSE 'error' END,'knowledge_model',COALESCE(p_record->'metadata','{}'::jsonb)||jsonb_build_object('pricing_snapshot',c.pricing,'knowledge_model_receipt',p_result->'receipt','provider_request_id',c.provider_request_id),(p_record->>'timestamp')::timestamptz);
 UPDATE gateway.knowledge_model_calls SET state=CASE WHEN p_result->>'state'='completed' THEN 'completed' ELSE 'rejected' END,result=p_result,completed_at=now() WHERE lease_id=p_lease;
 RETURN p_result;
END $$;
REVOKE ALL ON gateway.knowledge_model_calls,gateway.knowledge_model_workload_nonces FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.reserve_knowledge_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.settle_knowledge_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb) FROM PUBLIC;
-- Authority grants to the measured gateway role ONLY:
-- SELECT on calls; UPDATE(state,dispatched_at,provider_request_id,error_code,observation)
-- on calls; INSERT on nonces; EXECUTE both functions. No Knowledge role grant.
-- Existing Gateway account/RBAC/model/price-book SELECT is required.
-- No Gateway access to Knowledge tables: scope checks use signed Knowledge RPC.
-- Nonce expiration cleanup and evidence-based unknown reconciliation belong to
-- the privileged authority, never a caller-supplied replay or automatic retry.
-- END FIXED SOURCE knowledge-model-plane

-- BEGIN FIXED SOURCE dispatch-token-reservations: database/proposals/dispatch-token-reservations.sql
-- Adds a minimal Gateway dispatch reservation ledger using the 026 quota accounting checkpoint.
-- Frozen source SHA-256: 94daf9090175a42edf630af9ac75be4ef81482da7f7ae0ae0830a17f618423f0
-- UNNUMBERED / NOT ACTIVATED: U-GW-07 dispatch admission lifecycle only.
-- Requires the canonical price-book-v2 accounting checkpoint proposal activated
-- first (including quota_counters transaction receipts). No pricing arithmetic,
-- copied usage ledger, or fingerprint edits. Canonical numbering/activation and
-- exact scoped-role verification remain the native authority writer's task.
CREATE TABLE gateway.dispatch_token_reservations (
 tenant_id text NOT NULL CHECK(tenant_id<>''),
 user_id text NOT NULL CHECK(user_id<>''),
 request_id text NOT NULL CHECK(request_id<>''),
 service_id text NOT NULL,
 request_type text NOT NULL CHECK(request_type IN ('invoke','stream')),
 member uuid NOT NULL,
 logical_id uuid NOT NULL,
 estimated_tokens bigint NOT NULL CHECK(estimated_tokens>=0),
 state text NOT NULL CHECK(state IN ('prepared','dispatched','awaiting_checkpoint','settled','cancelled','expired','outcome_unknown')),
 prepare_deadline timestamptz NOT NULL,
 candidate_receipt_id text,
 settlement_candidate jsonb,
 settlement_observation jsonb,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,user_id,member),
 CHECK((state='settled')=(settlement_observation IS NOT NULL)),
 CHECK(settlement_observation IS NULL OR (
   settlement_observation->>'tenant_id'=tenant_id AND
   settlement_observation->>'user_id'=user_id AND
   settlement_observation->>'request_id'=request_id AND
   settlement_observation->>'service_id'=service_id AND
   settlement_observation->>'request_type'=request_type AND
   settlement_observation->>'status' IN ('success','error')) IS TRUE)
);
CREATE INDEX dispatch_token_reservations_live ON gateway.dispatch_token_reservations(tenant_id,user_id,created_at)
 WHERE state IN ('prepared','dispatched','awaiting_checkpoint','outcome_unknown');
REVOKE ALL ON gateway.dispatch_token_reservations FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user NOT LIKE '%\_owner' ESCAPE '\' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
 THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.dispatch_token_reservations TO %I',principal);
 EXECUTE format('GRANT UPDATE(state,candidate_receipt_id,settlement_candidate,settlement_observation,updated_at) ON gateway.dispatch_token_reservations TO %I',principal);
END
$grants$;
-- GatewayDurable, Knowledge, Runtime/Worker receive no new privileges.
-- END FIXED SOURCE dispatch-token-reservations

-- BEGIN FIXED SOURCE enterprise-identity: database/proposals/enterprise-identity.sql
-- Adds tenant-scoped identity, SCIM, MFA, passkey and session-assurance state on existing Gateway identities.
-- Frozen source SHA-256: 839102b978eec643828a0e841bb98124dc4838ebd4461f8bca875b7db0721f5f
-- UNNUMBERED, UNACTIVATED canonical proposal for U-GW-04.
-- Metadata augments gateway.users/user_roles and the existing Redis sessions;
-- no parallel account, role, permission, or authenticated-session authority.
-- Add the constant default first; validate the explicit proof before setting NOT NULL.
ALTER TABLE gateway.users ADD COLUMN auth_epoch bigint DEFAULT 0;
ALTER TABLE gateway.users ADD CONSTRAINT enterprise_users_auth_epoch_v2_check CHECK(auth_epoch IS NOT NULL AND auth_epoch>=0) NOT VALID;
ALTER TABLE gateway.users VALIDATE CONSTRAINT enterprise_users_auth_epoch_v2_check;
ALTER TABLE gateway.users ALTER COLUMN auth_epoch SET NOT NULL;
CREATE TABLE gateway.enterprise_tenant_policy (
 tenant_id varchar(255) PRIMARY KEY REFERENCES gateway.tenants(tenant_id),
 require_mfa boolean NOT NULL DEFAULT false
);
CREATE TABLE gateway.enterprise_identity_links (
 tenant_id varchar(255) NOT NULL, provider_id varchar(255) NOT NULL,
 issuer text NOT NULL, subject text NOT NULL CHECK(length(subject) BETWEEN 1 AND 2048),
 user_id varchar(255) NOT NULL, created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,provider_id,issuer,subject),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_scim_credentials (
 credential_id uuid PRIMARY KEY,tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),
 token_hash char(64) UNIQUE NOT NULL CHECK(token_hash ~ '^[0-9a-f]{64}$'),
 scopes text[] NOT NULL CHECK(cardinality(scopes)>0), created_by varchar(255) NOT NULL,
 expires_at timestamptz NOT NULL,revoked_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE gateway.enterprise_scim_users (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 external_id varchar(255) NOT NULL,version bigint NOT NULL DEFAULT 1 CHECK(version>0),
 deleted boolean NOT NULL DEFAULT false,
 PRIMARY KEY(tenant_id,user_id),UNIQUE(tenant_id,external_id),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_scim_groups (
 tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),group_id uuid NOT NULL,
 external_id varchar(255) NOT NULL,display_name varchar(255) NOT NULL,
 version bigint NOT NULL DEFAULT 1 CHECK(version>0),PRIMARY KEY(tenant_id,group_id),UNIQUE(tenant_id,external_id)
);
CREATE TABLE gateway.enterprise_scim_members (
 tenant_id varchar(255) NOT NULL,group_id uuid NOT NULL,user_id varchar(255) NOT NULL,
 PRIMARY KEY(tenant_id,group_id,user_id),
 FOREIGN KEY(tenant_id,group_id) REFERENCES gateway.enterprise_scim_groups(tenant_id,group_id) ON DELETE CASCADE,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.enterprise_scim_users(tenant_id,user_id) ON DELETE RESTRICT
);
-- Operator-reviewed mappings only. SCIM displayName never creates a role grant.
CREATE TABLE gateway.enterprise_scim_role_map (
 tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),external_group_id varchar(255) NOT NULL,
 role_name varchar(100) NOT NULL REFERENCES gateway.rbac_roles(role_name) CHECK(lower(role_name) NOT IN ('admin','owner','superadmin','super_admin','platform_admin')),PRIMARY KEY(tenant_id,external_group_id,role_name)
);
CREATE TABLE gateway.enterprise_security_state (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,user_handle uuid UNIQUE NOT NULL,
 mfa_required boolean NOT NULL DEFAULT false,recovery_epoch bigint NOT NULL DEFAULT 0,
 PRIMARY KEY(tenant_id,user_id),FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_passkeys (
 credential_id text PRIMARY KEY,tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 passkey jsonb NOT NULL,counter bigint NOT NULL CHECK(counter BETWEEN 0 AND 4294967295),
 revision bigint NOT NULL DEFAULT 1 CHECK(revision>0),created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE INDEX enterprise_passkeys_owner ON gateway.enterprise_passkeys(tenant_id,user_id);
CREATE TABLE gateway.enterprise_recovery_codes (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,code_hash char(64) NOT NULL,
 consumed_at timestamptz,created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,user_id,code_hash),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
-- Assurance is evidence attached to an existing allowlist JTI, never standalone
-- session authority. Current user/tenant/revision and Redis revocation still win.
CREATE TABLE gateway.enterprise_session_assurance (
 jti uuid PRIMARY KEY,tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 method varchar(32) NOT NULL CHECK(method IN ('oidc','oidc_mfa','passkey','recovery')),
 verified_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,expires_at timestamptz NOT NULL,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
REVOKE ALL ON gateway.enterprise_tenant_policy,gateway.enterprise_identity_links,gateway.enterprise_scim_credentials,
 gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_scim_members,
 gateway.enterprise_scim_role_map,gateway.enterprise_security_state,gateway.enterprise_passkeys,
 gateway.enterprise_recovery_codes,gateway.enterprise_session_assurance FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));principal text;
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix';END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.enterprise_identity_links,gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_scim_members,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes,gateway.enterprise_session_assurance TO %I',principal);
 EXECUTE format('GRANT UPDATE ON gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I',principal);
 EXECUTE format('GRANT DELETE ON gateway.enterprise_scim_members,gateway.enterprise_scim_groups,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I',principal);
 EXECUTE format('GRANT SELECT ON gateway.enterprise_scim_role_map,gateway.enterprise_tenant_policy TO %I',principal);
END $grants$;
-- Epoch never stands in for password_changed_at. Actual password-revision,
-- status and legacy grant mutations advance it; bcrypt representation-only
-- rehash leaves password_changed_at unchanged and does not revoke sessions.
CREATE FUNCTION gateway.enterprise_user_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF NEW.auth_epoch<OLD.auth_epoch THEN RAISE EXCEPTION 'authentication epoch cannot decrease';END IF;
 IF NEW.password_changed_at IS DISTINCT FROM OLD.password_changed_at
 OR NEW.status IS DISTINCT FROM OLD.status OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
 OR NEW.roles IS DISTINCT FROM OLD.roles OR NEW.permissions IS DISTINCT FROM OLD.permissions
 THEN NEW.auth_epoch:=GREATEST(NEW.auth_epoch,OLD.auth_epoch+1);END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER enterprise_user_epoch BEFORE UPDATE ON gateway.users FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_user_epoch();
CREATE FUNCTION gateway.enterprise_assignment_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id=OLD.user_id;END IF;
 IF TG_OP='INSERT' OR (TG_OP='UPDATE' AND NEW.user_id IS DISTINCT FROM OLD.user_id)
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id=NEW.user_id;END IF;
 IF TG_OP='DELETE' THEN RETURN OLD;ELSE RETURN NEW;END IF;
END $$;
CREATE TRIGGER enterprise_role_assignment_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.user_roles FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_assignment_epoch();
CREATE TRIGGER enterprise_direct_permission_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.user_permissions FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_assignment_epoch();
CREATE FUNCTION gateway.enterprise_role_policy_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id IN(SELECT user_id FROM gateway.user_roles WHERE role_name=OLD.role_name);END IF;
 IF TG_OP='INSERT' OR (TG_OP='UPDATE' AND NEW.role_name IS DISTINCT FROM OLD.role_name)
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id IN(SELECT user_id FROM gateway.user_roles WHERE role_name=NEW.role_name);END IF;
 IF TG_OP='DELETE' THEN RETURN OLD;ELSE RETURN NEW;END IF;
END $$;
CREATE TRIGGER enterprise_role_permission_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.role_permissions FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_role_policy_epoch();
CREATE TRIGGER enterprise_role_definition_epoch AFTER UPDATE OF permissions ON gateway.rbac_roles FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_role_policy_epoch();
CREATE FUNCTION gateway.enterprise_tenant_mfa_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP='INSERT' OR NEW.require_mfa IS DISTINCT FROM OLD.require_mfa
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=NEW.tenant_id;END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER enterprise_tenant_mfa_epoch AFTER INSERT OR UPDATE ON gateway.enterprise_tenant_policy FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_tenant_mfa_epoch();
REVOKE ALL ON FUNCTION gateway.enterprise_user_epoch(),gateway.enterprise_assignment_epoch(),gateway.enterprise_role_policy_epoch(),gateway.enterprise_tenant_mfa_epoch() FROM PUBLIC;
-- END FIXED SOURCE enterprise-identity

-- BEGIN FIXED SOURCE routing-policy: database/proposals/routing-policy.sql
-- Adds versioned tenant routing policies and append-only routing decisions without provider or budget authority.
-- Frozen source SHA-256: 769b8d971da17d2caa151c4895e4a7110c2887ad6752cabe04ca5cc6be520f6a
-- Inactive authoring proposal. Native canonical authority assigns the successor,
-- measures raw fingerprints/ACL deltas and activates; do not execute out of band.
-- Versioned policy routing adds no provider/catalog/budget/accounting authority.
CREATE TABLE gateway.routing_policy_versions (
 tenant_id varchar(64) NOT NULL REFERENCES gateway.tenants(tenant_id),
 policy_id varchar(128) NOT NULL,
 version varchar(128) NOT NULL,
 pin char(64) NOT NULL CHECK(pin ~ '^[0-9a-f]{64}$'),
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 created_by varchar(64) NOT NULL,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,policy_id,version),
 UNIQUE(tenant_id,policy_id,pin)
);
CREATE TABLE gateway.routing_policy_active (
 tenant_id varchar(64) NOT NULL,
 policy_id varchar(128) NOT NULL,
 pin char(64) NOT NULL,
 PRIMARY KEY(tenant_id,policy_id),
 FOREIGN KEY(tenant_id,policy_id,pin) REFERENCES gateway.routing_policy_versions(tenant_id,policy_id,pin)
);
CREATE TABLE gateway.routing_decisions (
 tenant_id varchar(64) NOT NULL,
 actor_id varchar(64) NOT NULL,
 invocation_id varchar(512) NOT NULL,
 decision_id char(64) NOT NULL CHECK(decision_id ~ '^[0-9a-f]{64}$'),
 policy_id varchar(128) NOT NULL,
 policy_pin char(64) NOT NULL,
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,decision_id),
 UNIQUE(tenant_id,actor_id,invocation_id),
 FOREIGN KEY(tenant_id,policy_id,policy_pin) REFERENCES gateway.routing_policy_versions(tenant_id,policy_id,pin)
);
-- Append-only versions/decisions: Gateway gets neither UPDATE nor DELETE.
REVOKE ALL ON gateway.routing_policy_versions,gateway.routing_policy_active,gateway.routing_decisions FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.routing_policy_versions,gateway.routing_decisions TO %I',principal);
 EXECUTE format('GRANT SELECT,INSERT,UPDATE ON gateway.routing_policy_active TO %I',principal);
END $grants$;
-- No Runtime/Worker/Knowledge grants and no SECURITY DEFINER authority.
-- END FIXED SOURCE routing-policy

-- The Gateway is the only caller of the typed knowledge-model boundary.
DO $knowledge_model_grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT ON gateway.knowledge_model_calls TO %I',principal);
 EXECUTE format('GRANT UPDATE(state,dispatched_at,provider_request_id,error_code,observation) ON gateway.knowledge_model_calls TO %I',principal);
 EXECUTE format('GRANT INSERT ON gateway.knowledge_model_workload_nonces TO %I',principal);
 EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.reserve_knowledge_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint) TO %I',principal);
 EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.settle_knowledge_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb) TO %I',principal);
END
$knowledge_model_grants$;
