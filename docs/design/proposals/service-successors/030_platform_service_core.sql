-- INACTIVE source-owned030 service core. Native authority owns the transaction.
DO $service_owner$ BEGIN IF (current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')) IS NOT TRUE THEN RAISE EXCEPTION 'canonical scoped schema owner required'; END IF; END $service_owner$;
-- UNNUMBERED PROPOSAL. Apply only through native migration authority, with an
-- admitted schema epoch and scoped runtime EXECUTE grants. Do not run directly.
-- Existing execution-owner functions/frozen fingerprints are not edited.
CREATE TABLE assistant.assistant_runtime_thread_owners (
    runtime_thread_id uuid PRIMARY KEY REFERENCES assistant.assistant_runtime_threads(runtime_thread_id),
    owner_id uuid NOT NULL,
    address text NOT NULL CHECK (length(address) BETWEEN 1 AND 2048),
    fence bigint NOT NULL CHECK (fence > 0),
    lease_until timestamptz NOT NULL,
    heartbeat_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON assistant.assistant_runtime_thread_owners FROM PUBLIC;

CREATE FUNCTION assistant.claim_runtime_thread_owner(p_thread uuid,p_owner uuid,p_address text)
RETURNS assistant.assistant_runtime_thread_owners
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp AS $$
DECLARE claim assistant.assistant_runtime_thread_owners%ROWTYPE;
BEGIN
    IF length(p_address) NOT BETWEEN 1 AND 2048 THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_ADDRESS_INVALID' USING ERRCODE='22023';
    END IF;
    PERFORM 1 FROM assistant_runtime_threads WHERE runtime_thread_id=p_thread AND deleted_at IS NULL FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_THREAD_NOT_FOUND' USING ERRCODE='55000'; END IF;
    SELECT * INTO claim FROM assistant_runtime_thread_owners WHERE runtime_thread_id=p_thread FOR UPDATE;
    IF FOUND THEN
        -- A valid execution lease remains authoritative even at the boundary
        -- where the thread heartbeat has not yet caught up. Never steal it.
        IF claim.owner_id<>p_owner AND (claim.lease_until>clock_timestamp() OR EXISTS (
            SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs r ON r.run_id=o.run_id
            WHERE o.runtime_thread_id=p_thread AND o.owner_id<>p_owner
              AND o.lease_until>clock_timestamp() AND r.status IN ('running','awaiting_approval')
        )) THEN RETURN claim; END IF;
        IF claim.owner_id=p_owner AND claim.address<>p_address THEN
            RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_ADDRESS_CHANGED' USING ERRCODE='42501';
        END IF;
        UPDATE assistant_runtime_thread_owners SET owner_id=p_owner,address=p_address,
            fence=fence+CASE WHEN owner_id=p_owner THEN 0 ELSE 1 END,
            lease_until=clock_timestamp()+INTERVAL '30 seconds',heartbeat_at=clock_timestamp()
            WHERE runtime_thread_id=p_thread RETURNING * INTO claim;
    ELSE
        -- Mixed old/new runtime ownership cannot invent the old owner's URL.
        IF EXISTS (SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs r ON r.run_id=o.run_id
            WHERE o.runtime_thread_id=p_thread AND o.owner_id<>p_owner AND o.lease_until>clock_timestamp()
              AND r.status IN ('running','awaiting_approval')) THEN
            RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_ADDRESS_UNKNOWN' USING ERRCODE='55000';
        END IF;
        INSERT INTO assistant_runtime_thread_owners(runtime_thread_id,owner_id,address,fence,lease_until)
            VALUES(p_thread,p_owner,p_address,1,clock_timestamp()+INTERVAL '30 seconds') RETURNING * INTO claim;
    END IF;
    RETURN claim;
END $$;

CREATE FUNCTION assistant.assert_runtime_thread_owner_for_run(p_run uuid,p_owner uuid,p_address text)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp AS $$
DECLARE r assistant.assistant_runs%ROWTYPE;
    claim assistant.assistant_runtime_thread_owners%ROWTYPE;
BEGIN
    -- Preserve the existing global lock order: run -> root thread -> owner.
    SELECT * INTO r FROM assistant_runs WHERE run_id=p_run FOR UPDATE;
    IF NOT FOUND OR r.engine<>'agent_runtime' OR r.status NOT IN ('running','awaiting_approval') THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_RUN_NOT_ACTIVE' USING ERRCODE='55000';
    END IF;
    SELECT * INTO claim FROM claim_runtime_thread_owner(r.harness_thread_id,p_owner,p_address);
    IF claim.owner_id<>p_owner OR claim.address<>p_address OR claim.lease_until<=clock_timestamp() THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_LOST' USING ERRCODE='42501';
    END IF;
    RETURN true;
END $$;
CREATE FUNCTION assistant.heartbeat_runtime_thread_owner(p_thread uuid,p_owner uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,pg_temp AS $$
BEGIN
    UPDATE assistant_runtime_thread_owners SET lease_until=clock_timestamp()+INTERVAL '30 seconds',heartbeat_at=clock_timestamp()
      WHERE runtime_thread_id=p_thread AND owner_id=p_owner AND lease_until>clock_timestamp();
    RETURN FOUND;
END $$;
REVOKE ALL ON FUNCTION assistant.claim_runtime_thread_owner(uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.assert_runtime_thread_owner_for_run(uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.heartbeat_runtime_thread_owner(uuid,uuid) FROM PUBLIC;
-- Authority integration must assign functions/table to canonical non-login owner;
-- runtime gets only SELECT(table), EXECUTE(the three functions); no direct writes.
-- Add exact ownership/ACL and positive/negative scoped role witnesses before activation.

-- PROPOSAL ONLY. Unnumbered: native authority must admit/measure and grant roles.
-- Execute as migration owner, never SDK worker or runtime. No existing migration edited.
CREATE TABLE gateway.sdk_model_calls (
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
CREATE FUNCTION gateway.reserve_sdk_model_call(
 p_tenant text,p_user text,p_call text,p_hash text,p_lease uuid,p_token jsonb,p_body jsonb,p_snapshot jsonb,p_price jsonb,p_input bigint,p_output bigint,p_cost bigint
) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway,pg_temp AS $$
DECLARE sdk assistant.assistant_sdk_runs; existing gateway.sdk_model_calls; expired gateway.sdk_model_calls; q gateway.user_quotas; tokens bigint; cents bigint; refund bigint; cost_nanos numeric; spent_nanos numeric; pending_nanos numeric; period_start timestamptz;
BEGIN
 IF p_input<0 OR p_output<0 OR p_cost<0 OR p_hash !~ '^[0-9a-f]{64}$' OR p_token->>'tenant_id'<>p_tenant OR p_token->>'user_id'<>p_user OR p_token->>'call_id'<>p_call OR p_token->>'request_sha256'<>p_hash OR p_body->'scope'->'actor'->>'tenant_id'<>p_tenant OR p_body->'scope'->'actor'->>'user_id'<>p_user THEN RAISE EXCEPTION 'SDK_MODEL_INVALID'; END IF;
 PERFORM 1 FROM assistant.assistant_runs WHERE run_id=(p_body->>'sdk_run_id')::uuid FOR SHARE;
 SELECT * INTO sdk FROM assistant.assistant_sdk_runs WHERE run_id=(p_body->>'sdk_run_id')::uuid FOR UPDATE;
 IF NOT FOUND OR sdk.tenant_id<>p_tenant OR sdk.user_id<>p_user
    OR sdk.owner_id::text<>p_body->>'owner_id' OR sdk.fence<>(p_body->>'fence')::bigint
    OR sdk.snapshot->'sdk_agent'->>'model'<>p_body->>'model' THEN
    RAISE EXCEPTION 'SDK_MODEL_RUN_BINDING_INVALID';
 END IF;
 PERFORM assistant.assert_sdk_execution(sdk.run_id,p_tenant,p_user,sdk.session_id,sdk.owner_id,sdk.fence,TRUE);
 IF (SELECT count(*) FROM gateway.sdk_model_calls WHERE call->>'sdk_run_id'=sdk.run_id::text)>=LEAST(32,(sdk.snapshot->'sdk_agent'->>'max_turns')::integer)
    AND NOT EXISTS (SELECT 1 FROM gateway.sdk_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND call_id=p_call) THEN
    RAISE EXCEPTION 'SDK_MODEL_RUN_BUDGET_EXHAUSTED';
 END IF;
 -- Per-actor quota row serializes reservations AND duplicate first creation.
 SELECT * INTO q FROM gateway.user_quotas WHERE tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'SDK_MODEL_QUOTA_NOT_PROVISIONED'; END IF;
 SELECT * INTO existing FROM gateway.sdk_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND call_id=p_call;
 IF FOUND THEN
  IF existing.request_sha256<>p_hash OR existing.call<>p_body THEN RAISE EXCEPTION 'SDK_MODEL_BODY_CONFLICT'; END IF;
  RETURN to_jsonb(existing);
 END IF;
 -- Only expired never-dispatched reservations can be safely released. The
 -- quota lock and reserved-state row lock serialize this against dispatch CAS.
 FOR expired IN SELECT * FROM gateway.sdk_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND state='reserved' AND expires_at<=NOW() FOR UPDATE LOOP
  refund:=(expired.reserved_cost_microcents/10000)+CASE WHEN expired.reserved_cost_microcents%10000>0 THEN 1 ELSE 0 END;
  UPDATE gateway.user_quotas SET
   current_daily_tokens=GREATEST(0,COALESCE(current_daily_tokens,0)-CASE WHEN daily_reset_at IS NOT DISTINCT FROM expired.quota_daily_period THEN expired.reserved_input+expired.reserved_output ELSE 0 END),
   current_monthly_tokens=GREATEST(0,COALESCE(current_monthly_tokens,0)-CASE WHEN monthly_reset_at IS NOT DISTINCT FROM expired.quota_monthly_period THEN expired.reserved_input+expired.reserved_output ELSE 0 END),
   current_monthly_cost_cents=GREATEST(0,COALESCE(current_monthly_cost_cents,0)-CASE WHEN monthly_reset_at IS NOT DISTINCT FROM expired.quota_monthly_period THEN refund ELSE 0 END),
   current_daily_requests=GREATEST(0,COALESCE(current_daily_requests,0)-CASE WHEN daily_reset_at IS NOT DISTINCT FROM expired.quota_daily_period THEN 1 ELSE 0 END)
  WHERE id=q.id;
  UPDATE gateway.sdk_model_calls SET state='rejected',error_code='reservation_expired',completed_at=NOW(),
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
 IF cost_nanos IS NULL OR cost_nanos<0 THEN RAISE EXCEPTION 'SDK_MODEL_INVALID_PRICE'; END IF;
 period_start:=CASE WHEN q.monthly_reset_at>NOW() THEN q.monthly_reset_at-interval '1 month' ELSE COALESCE(q.monthly_reset_at,date_trunc('month',NOW())) END;
 SELECT COALESCE(SUM(CASE WHEN metadata->'measured_units'->>'meter'='requests' THEN (metadata->>'request_cost_nanos')::numeric ELSE total_cost_cents::numeric*1000 END),0) INTO spent_nanos FROM gateway.usage_records WHERE tenant_id=p_tenant AND user_id=p_user AND created_at>=period_start;
 SELECT COALESCE(SUM(reserved_cost_nanos),0) INTO pending_nanos FROM gateway.sdk_model_calls WHERE tenant_id=p_tenant AND user_id=p_user AND state IN ('reserved','dispatched','observed','unknown') AND quota_monthly_period IS NOT DISTINCT FROM q.monthly_reset_at;
 IF COALESCE(q.is_blocked,FALSE)
 OR (q.daily_token_limit IS NOT NULL AND COALESCE(q.current_daily_tokens,0)+tokens>q.daily_token_limit)
 OR (q.monthly_token_limit IS NOT NULL AND COALESCE(q.current_monthly_tokens,0)+tokens>q.monthly_token_limit)
 OR (q.monthly_cost_limit_cents IS NOT NULL AND COALESCE(q.current_monthly_cost_cents,0)+cents>q.monthly_cost_limit_cents)
 OR (q.monthly_cost_limit_cents IS NOT NULL AND spent_nanos+pending_nanos+cost_nanos>q.monthly_cost_limit_cents::numeric*10000000)
 OR (q.requests_per_day IS NOT NULL AND COALESCE(q.current_daily_requests,0)+1>q.requests_per_day)
 THEN RAISE EXCEPTION 'SDK_MODEL_QUOTA_EXHAUSTED'; END IF;
 UPDATE gateway.user_quotas SET current_daily_tokens=COALESCE(current_daily_tokens,0)+tokens,current_monthly_tokens=COALESCE(current_monthly_tokens,0)+tokens,current_monthly_cost_cents=COALESCE(current_monthly_cost_cents,0)+cents,current_daily_requests=COALESCE(current_daily_requests,0)+1 WHERE id=q.id;
 INSERT INTO gateway.sdk_model_calls(lease_id,tenant_id,user_id,call_id,request_sha256,lease,call,snapshot,pricing,reserved_input,reserved_output,reserved_cost_microcents,reserved_cost_nanos,quota_daily_period,quota_monthly_period,expires_at)
 VALUES(p_lease,p_tenant,p_user,p_call,p_hash,p_token,p_body,p_snapshot,p_price,p_input,p_output,p_cost,cost_nanos,q.daily_reset_at,q.monthly_reset_at,to_timestamp((p_token->>'expires_at_unix')::double precision)) RETURNING * INTO existing;
 RETURN to_jsonb(existing);
END $$;
CREATE FUNCTION gateway.settle_sdk_model_call(p_lease uuid,p_tenant text,p_user text,p_input bigint,p_output bigint,p_input_cost bigint,p_output_cost bigint,p_result jsonb,p_record jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway,pg_temp AS $$
DECLARE c gateway.sdk_model_calls; q gateway.user_quotas; tokens bigint; reserved_cents bigint; actual_cents bigint;
BEGIN
 -- Same lock order as reserve. The call row is immutable in identity.
 SELECT * INTO q FROM gateway.user_quotas WHERE tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'SDK_MODEL_QUOTA_NOT_PROVISIONED'; END IF;
 SELECT * INTO c FROM gateway.sdk_model_calls WHERE lease_id=p_lease AND tenant_id=p_tenant AND user_id=p_user FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'SDK_MODEL_NOT_FOUND'; END IF;
 IF c.result IS NOT NULL THEN RETURN c.result; END IF;
 IF c.state NOT IN ('observed','unknown') OR c.observation IS NULL OR p_input<0 OR p_output<0 OR p_input_cost<0 OR p_output_cost<0
 OR p_result->>'lease_id'<>p_lease::text OR p_result->>'request_sha256'<>c.request_sha256 OR p_result->'receipt'->>'tenant_id'<>p_tenant OR p_result->'receipt'->>'user_id'<>p_user OR p_result->'receipt'->>'settled'<>'true' THEN RAISE EXCEPTION 'SDK_MODEL_RECEIPT_INVALID'; END IF;
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
 VALUES(p_tenant,p_user,p_record->>'request_id',p_record->>'service_id',c.lease->'model'->>'model',c.lease->'model'->>'provider',CASE WHEN p_record->'metadata'->'measured_units'->>'meter'='requests' THEN NULL ELSE p_input END,CASE WHEN p_record->'metadata'->'measured_units'->>'meter'='requests' THEN NULL ELSE p_output END,p_input_cost,p_output_cost,
 CASE WHEN p_result->>'state'='completed' THEN 'success' ELSE 'error' END,'sdk_model',COALESCE(p_record->'metadata','{}'::jsonb)||jsonb_build_object('pricing_snapshot',c.pricing,'sdk_model_receipt',p_result->'receipt','provider_request_id',c.provider_request_id),(p_record->>'timestamp')::timestamptz);
 UPDATE gateway.sdk_model_calls SET state=CASE WHEN p_result->>'state'='completed' THEN 'completed' ELSE 'rejected' END,result=p_result,completed_at=now() WHERE lease_id=p_lease;
 RETURN p_result;
END $$;
REVOKE ALL ON gateway.sdk_model_calls FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.reserve_sdk_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.settle_sdk_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb) FROM PUBLIC;
-- Authority grants to the measured gateway role ONLY:
-- SELECT on calls; UPDATE(state,dispatched_at,provider_request_id,error_code,observation)
-- on calls; EXECUTE both functions. No SDK worker role grant.
-- Existing Gateway account/RBAC/model/price-book SELECT is required.
-- SDK authority uses existing run/grant lineage; no managed Runtime run is created.
-- Nonce expiration cleanup and evidence-based unknown reconciliation belong to
-- the privileged authority, never a caller-supplied replay or automatic retry.
-- Payload-free push hints. Same transaction as revocation or terminal state.
CREATE FUNCTION assistant.notify_sdk_execution_revocation() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,assistant,pg_temp AS $$
BEGIN
 PERFORM pg_notify('sdk_execution_revocations_v1','wake');
 RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION assistant.notify_sdk_execution_revocation() FROM PUBLIC;
CREATE TRIGGER sdk_execution_revocation_push AFTER UPDATE OF revoked_at,lease_until ON assistant.assistant_sdk_runs
 FOR EACH STATEMENT EXECUTE FUNCTION assistant.notify_sdk_execution_revocation();
CREATE TRIGGER sdk_execution_terminal_push AFTER UPDATE OF status ON assistant.assistant_runs
 FOR EACH STATEMENT EXECUTE FUNCTION assistant.notify_sdk_execution_revocation();
-- Add authority-owned triggers on actor/session/service policy publication in the
-- admitted epoch. The live grant check handles every wake; hints grant no authority.

-- UNNUMBERED PROPOSAL ONLY. Apply only through database authority after review.
-- No bootstrap/runtime DDL and no frozen schema/fingerprint modifications.
CREATE TABLE knowledge.pipeline_work (
 document_id varchar PRIMARY KEY REFERENCES knowledge.documents(document_id),
 dataset_id varchar NOT NULL REFERENCES knowledge.datasets(dataset_id), tenant_id varchar NOT NULL,
 job_id uuid NOT NULL UNIQUE, execution_id varchar,
 phase text NOT NULL CHECK (phase IN ('worker','parse','embed','index')),
 status text NOT NULL CHECK (status IN ('ready','running','completed','error')),
 owner_token text, fence bigint NOT NULL DEFAULT 0 CHECK (fence>=0),
 lease_expires_at timestamptz, checkpoints jsonb NOT NULL DEFAULT '{}' CHECK(jsonb_typeof(checkpoints)='object'),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 CHECK ((status='running')=(owner_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);
CREATE INDEX pipeline_work_dispatch ON knowledge.pipeline_work(phase,status,updated_at);
-- Grants/RLS must use the native authority's actual Knowledge API/worker roles.
-- API role: no writes/claims. Worker role: tenant-authorized work + CAS only.

-- Install through a NEW native-authority successor; no application startup DDL.
CREATE TABLE gateway.authorization_decisions (
    decision_id uuid PRIMARY KEY,
    capability text NOT NULL CHECK (length(capability) BETWEEN 1 AND 128),
    policy_sha256 text NOT NULL CHECK (policy_sha256 ~ '^[0-9a-f]{64}$'),
    matrix_sha256 text NOT NULL CHECK (matrix_sha256 ~ '^[0-9a-f]{64}$'),
    principal_fingerprint text NOT NULL CHECK (principal_fingerprint ~ '^[0-9a-f]{64}$'),
    mode text NOT NULL CHECK (mode IN ('shadow','enforce')),
    legacy_allowed boolean NOT NULL,
    cedar_allowed boolean NOT NULL,
    enforced_allowed boolean NOT NULL,
    evaluation_error boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON gateway.authorization_decisions FROM PUBLIC;
-- Append-only process role. Audit readers use a separately governed owner/admin
-- path; possession of Gateway credentials grants no audit SELECT/UPDATE/DELETE.
-- Dedicated prefix-derived audit INSERT is appended by native authority.

-- New successor only; do not alter frozen 029 or its fingerprint.
CREATE TABLE gateway.authorization_resource_decisions (
 decision_id uuid PRIMARY KEY,capability varchar(128) NOT NULL,
 policy_sha256 text NOT NULL CHECK(policy_sha256~'^[0-9a-f]{64}$'),
 matrix_sha256 text NOT NULL CHECK(matrix_sha256~'^[0-9a-f]{64}$'),
 principal_fingerprint text NOT NULL CHECK(principal_fingerprint~'^[0-9a-f]{64}$'),
 mode text NOT NULL CHECK(mode IN ('shadow','enforce')),
 legacy_allowed boolean NOT NULL,cedar_allowed boolean NOT NULL,
 enforced_allowed boolean NOT NULL,evaluation_error boolean NOT NULL,
 resource_context jsonb NOT NULL CHECK(jsonb_typeof(resource_context)='object'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX authorization_resource_decisions_created ON gateway.authorization_resource_decisions(created_at);
CREATE INDEX authorization_resource_decisions_differences ON gateway.authorization_resource_decisions(created_at) WHERE legacy_allowed<>cedar_allowed;
REVOKE ALL ON gateway.authorization_resource_decisions FROM PUBLIC;
-- Native authority renders its gateway app role: GRANT INSERT only. Operator
-- audit read access stays separately authorized; no UPDATE/DELETE app privilege.

-- UNNUMBERED proposal: install through the native authority as a new successor.
-- Search path must resolve these exclusively to the gateway schema.
CREATE TABLE gateway.gateway_a2a_contexts (
 context_id uuid PRIMARY KEY, tenant_id text NOT NULL, user_id text NOT NULL,
 publication_id uuid NOT NULL, version_id uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT NOW(),
 UNIQUE(context_id,tenant_id,user_id,publication_id)
);
CREATE TABLE gateway.gateway_a2a_tasks (
 task_id uuid PRIMARY KEY, context_id uuid NOT NULL, tenant_id text NOT NULL, user_id text NOT NULL,
 publication_id uuid NOT NULL, message_id uuid NOT NULL, request_hash text NOT NULL CHECK(request_hash~'^[0-9a-f]{64}$'),
 effect_state text NOT NULL CHECK(effect_state IN ('reserved','dispatched','unknown','confirmed','rejected')),
 run_id uuid, thread_id uuid, created_at timestamptz NOT NULL DEFAULT NOW(),updated_at timestamptz NOT NULL DEFAULT NOW(),
 FOREIGN KEY(context_id,tenant_id,user_id,publication_id) REFERENCES gateway.gateway_a2a_contexts(context_id,tenant_id,user_id,publication_id),
 UNIQUE(tenant_id,user_id,publication_id,message_id)
);
CREATE INDEX gateway_a2a_context_effects ON gateway.gateway_a2a_tasks(context_id,effect_state);
CREATE TABLE gateway.gateway_a2a_outbound_effects (
 tenant_id text NOT NULL,user_id text NOT NULL,agent_name text NOT NULL,message_id uuid NOT NULL,
 request_hash text NOT NULL CHECK(request_hash~'^[0-9a-f]{64}$'),effect_state text NOT NULL CHECK(effect_state IN ('unknown','confirmed')),
 response jsonb,created_at timestamptz NOT NULL DEFAULT NOW(),updated_at timestamptz NOT NULL DEFAULT NOW(),
 PRIMARY KEY(tenant_id,user_id,agent_name,message_id)
);
CREATE INDEX gateway_a2a_outbound_daily ON gateway.gateway_a2a_outbound_effects(tenant_id,agent_name,created_at);
REVOKE ALL ON gateway.gateway_a2a_contexts,gateway.gateway_a2a_tasks,gateway.gateway_a2a_outbound_effects FROM PUBLIC;
-- Authority integrates least-privilege SELECT,INSERT,UPDATE for ai_gateway_app.
-- No DELETE privilege. Apply existing tenant isolation/role policy conventions.

REVOKE ALL ON assistant.assistant_runtime_thread_owners FROM PUBLIC;
REVOKE ALL ON gateway.sdk_model_calls FROM PUBLIC;
REVOKE ALL ON knowledge.pipeline_work FROM PUBLIC;
REVOKE ALL ON gateway.authorization_decisions FROM PUBLIC;
REVOKE ALL ON gateway.authorization_resource_decisions FROM PUBLIC;
REVOKE ALL ON gateway.gateway_a2a_contexts FROM PUBLIC;
REVOKE ALL ON gateway.gateway_a2a_tasks FROM PUBLIC;
REVOKE ALL ON gateway.gateway_a2a_outbound_effects FROM PUBLIC;
DO $service_grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical prefix'; END IF;
 EXECUTE format('GRANT SELECT ON assistant.assistant_runtime_thread_owners TO %I',prefix||'runtime');
 EXECUTE format('GRANT SELECT ON gateway.sdk_model_calls TO %I',prefix||'gateway');
 EXECUTE format('GRANT INSERT,SELECT ON knowledge.pipeline_work TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT INSERT ON gateway.authorization_decisions TO %I',prefix||'gateway');
 EXECUTE format('GRANT INSERT ON gateway.authorization_resource_decisions TO %I',prefix||'gateway');
 EXECUTE format('GRANT INSERT,SELECT ON gateway.gateway_a2a_contexts TO %I',prefix||'gateway');
 EXECUTE format('GRANT INSERT,SELECT ON gateway.gateway_a2a_tasks TO %I',prefix||'gateway');
 EXECUTE format('GRANT INSERT,SELECT ON gateway.gateway_a2a_outbound_effects TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(created_at) ON gateway.gateway_a2a_contexts TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(state,dispatched_at,provider_request_id,error_code,observation) ON gateway.sdk_model_calls TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(job_id,phase,status,execution_id,checkpoints,owner_token,lease_expires_at,fence,updated_at) ON knowledge.pipeline_work TO %I',prefix||'knowledge_worker');
 EXECUTE format('GRANT UPDATE(effect_state,run_id,thread_id,updated_at) ON gateway.gateway_a2a_tasks TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(effect_state,response,updated_at) ON gateway.gateway_a2a_outbound_effects TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.claim_runtime_thread_owner(uuid, uuid, text) TO %I',prefix||'runtime');
 EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.assert_runtime_thread_owner_for_run(uuid, uuid, text) TO %I',prefix||'runtime');
 EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.heartbeat_runtime_thread_owner(uuid, uuid) TO %I',prefix||'runtime');
 EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.reserve_sdk_model_call(text, text, text, text, uuid, jsonb, jsonb, jsonb, jsonb, bigint, bigint, bigint) TO %I',prefix||'gateway');
 EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.settle_sdk_model_call(uuid, text, text, bigint, bigint, bigint, bigint, jsonb, jsonb) TO %I',prefix||'gateway');
END $service_grants$;
