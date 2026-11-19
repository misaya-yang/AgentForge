-- UNACTIVATED, unnumbered canonical successor proposal. Depends on the
-- price-book-v2 proposal and an explicitly issued v2 snapshot. No legacy
-- completion function, receipt or role attributes are changed.
CREATE TABLE assistant.runtime_cache_settlement_v2 (
 call_id uuid PRIMARY KEY REFERENCES assistant.assistant_runtime_model_calls(call_id),
 tenant_id varchar(255) NOT NULL,
 receipt jsonb NOT NULL CHECK(receipt->>'schema'='agentforge.runtime-cache-settlement/v2'),
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE FUNCTION assistant.runtime_cache_settlement_v2_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $$
BEGIN RAISE EXCEPTION 'RUNTIME_CACHE_RECEIPT_IMMUTABLE' USING ERRCODE='55000'; END $$;
CREATE TRIGGER runtime_cache_settlement_v2_immutable BEFORE UPDATE OR DELETE OR TRUNCATE
ON assistant.runtime_cache_settlement_v2 FOR EACH STATEMENT
EXECUTE FUNCTION assistant.runtime_cache_settlement_v2_immutable();
CREATE FUNCTION assistant.complete_assistant_runtime_model_call_v2(
 p_call_id uuid,p_tenant_id varchar,p_input_tokens bigint,p_output_tokens bigint,
 p_cost_microusd bigint,p_provider_request_id varchar,p_receipt jsonb
) RETURNS void LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,assistant,pg_temp AS $$
DECLARE v_call assistant.assistant_runtime_model_calls%ROWTYPE; v_snapshot jsonb; v_old jsonb; v_pricing jsonb;
BEGIN
 SELECT * INTO v_call FROM assistant.assistant_runtime_model_calls
 WHERE call_id=p_call_id AND tenant_id=p_tenant_id FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_CACHE_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
 SELECT snapshot INTO STRICT v_snapshot FROM assistant.assistant_runtime_snapshots
 WHERE snapshot_id=v_call.snapshot_id AND tenant_id=p_tenant_id;
 v_pricing:=CASE WHEN v_snapshot#>'{pricing,price_book_schema}' IS NOT NULL THEN v_snapshot->'pricing' ELSE v_snapshot#>'{pricing,snapshot}' END;
 IF p_input_tokens IS NULL OR p_output_tokens IS NULL OR p_cost_microusd IS NULL
 OR p_input_tokens<0 OR p_output_tokens<0 OR p_cost_microusd<0
 OR p_receipt IS NULL OR jsonb_typeof(p_receipt)<>'object'
 OR p_receipt->>'schema' IS DISTINCT FROM 'agentforge.runtime-cache-settlement/v2'
 OR p_receipt->>'tenant_id' IS DISTINCT FROM p_tenant_id
 OR p_receipt->>'provider' IS DISTINCT FROM v_snapshot#>>'{model,provider_id}'
 OR p_receipt->>'model' IS DISTINCT FROM v_snapshot#>>'{model,id}'
 OR p_receipt->'input_tokens' IS DISTINCT FROM to_jsonb(p_input_tokens)
 OR p_receipt->'output_tokens' IS DISTINCT FROM to_jsonb(p_output_tokens)
 OR p_receipt->'cost_microusd' IS DISTINCT FROM to_jsonb(p_cost_microusd)
 OR p_receipt#>'{metadata,pricing_snapshot}' IS DISTINCT FROM v_pricing
 OR v_pricing->>'price_book_schema' IS DISTINCT FROM 'agentforge.price_book/v2'
 THEN RAISE EXCEPTION 'RUNTIME_CACHE_RECEIPT_INVALID' USING ERRCODE='22023'; END IF;
 SELECT receipt INTO v_old FROM assistant.runtime_cache_settlement_v2 WHERE call_id=p_call_id;
 IF FOUND THEN
   IF v_old IS DISTINCT FROM p_receipt OR v_call.provider_request_id IS DISTINCT FROM p_provider_request_id
   THEN RAISE EXCEPTION 'RUNTIME_CACHE_RECEIPT_CONFLICT' USING ERRCODE='55000'; END IF;
   RETURN; -- repeated completion cannot increment lease accounting twice
 END IF;
 PERFORM assistant.complete_assistant_runtime_model_call(p_call_id,p_input_tokens,p_output_tokens,p_cost_microusd,p_provider_request_id);
 INSERT INTO assistant.runtime_cache_settlement_v2(call_id,tenant_id,receipt) VALUES(p_call_id,p_tenant_id,p_receipt);
END $$;
REVOKE ALL ON assistant.runtime_cache_settlement_v2 FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.runtime_cache_settlement_v2_immutable() FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.complete_assistant_runtime_model_call_v2(uuid,varchar,bigint,bigint,bigint,varchar,jsonb) FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF;
 -- The canonical owner owns the table and definer; only Gateway executes the
 -- trusted typed settlement boundary. Runtime/Worker get no new table grants.
 EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.complete_assistant_runtime_model_call_v2(uuid,varchar,bigint,bigint,bigint,varchar,jsonb) TO %I',prefix||'gateway');
END $grants$;
