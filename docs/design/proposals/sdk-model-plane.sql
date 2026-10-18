-- PROPOSAL ONLY. Unnumbered: native authority must admit/measure and grant roles.
-- Execute as migration owner, never SDK worker or runtime. No existing migration edited.
BEGIN;
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
) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway AS $$
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
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,gateway AS $$
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
LANGUAGE plpgsql SET search_path=pg_catalog,assistant AS $$
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
COMMIT;
