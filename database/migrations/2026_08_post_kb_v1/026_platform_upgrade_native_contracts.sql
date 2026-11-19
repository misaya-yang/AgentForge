-- UNACTIVATED, unnumbered fixed source bundle for canonical successor 026.
-- Generated only from the five named proposal sources below, in this order.
-- The Runtime attenuation-witness table receives a fixed definer path and authority-resolved Gateway/Runtime grants.
-- No arbitrary SQL input, database action, release pin, or activation is included.

-- BEGIN FIXED SOURCE price-book-v2: database/proposals/price-book-v2.sql
-- Foundation: immutable price-book terms, publications, routing, receipts and audit.
-- Frozen source SHA-256: e384ba326a3d28f251f65ac74ae6a5fcec6877ffa7972ccf88298c12a246dd28
-- UNACTIVATED PROPOSAL; no successor number is allocated here.
-- Native authority alone allocates, measures, promotes and applies this source.
-- No frozen source, legacy price, receipt, fingerprint or lock is rewritten.
-- Gateway-only pricing authority. No new extension, public USAGE or runtime grant.
DO $owner_check$
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
    OR current_user<>(SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname='gateway') THEN
   RAISE EXCEPTION 'canonical Gateway owner scope required';
 END IF;
END
$owner_check$;

CREATE TABLE gateway.price_book_v2_terms (
 reference text PRIMARY KEY CHECK(reference ~ '^agentforge\.price_book/v2/(entry|discount)/[0-9a-f]{64}$'),
 kind text NOT NULL CHECK(kind IN ('entry','discount')),
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 tenant_id text GENERATED ALWAYS AS(payload->>'tenant') STORED,
 book_version text GENERATED ALWAYS AS(payload->'key'->>'book_version') STORED NOT NULL,
 provider_id text GENERATED ALWAYS AS(payload->'key'->>'provider') STORED NOT NULL,
 model_id text GENERATED ALWAYS AS(payload->'key'->>'model') STORED NOT NULL,
 model_version text GENERATED ALWAYS AS(payload->'key'->>'model_version') STORED NOT NULL,
 component text GENERATED ALWAYS AS(payload->'key'->>'component') STORED NOT NULL,
 currency text GENERATED ALWAYS AS(payload->'key'->>'currency') STORED NOT NULL,
 effective_from_ms bigint GENERATED ALWAYS AS((payload->'effective'->>'from_ms')::bigint) STORED NOT NULL,
 effective_to_ms bigint GENERATED ALWAYS AS((payload->'effective'->>'to_ms')::bigint) STORED,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 CHECK(tenant_id IS NULL OR tenant_id<>''),
 CHECK(book_version<>'' AND provider_id<>'' AND model_id<>'' AND model_version<>''),
 CHECK(component IN ('input','cached_input','cache_write','output','reasoning','tool_call','image','batch')),
 CHECK(currency ~ '^[A-Z]{3}$'),
 CHECK(effective_to_ms IS NULL OR effective_to_ms>effective_from_ms),
 CHECK((kind='entry' AND reference LIKE 'agentforge.price_book/v2/entry/%') OR (kind='discount' AND reference LIKE 'agentforge.price_book/v2/discount/%')),
 CHECK(kind<>'entry' OR ((payload->>'rate_nanos') ~ '^[0-9]+$' AND (payload->>'rate_nanos')::numeric<=9223372036854775807 AND (payload->>'units_per_rate') ~ '^[0-9]+$' AND (payload->>'units_per_rate')::numeric BETWEEN 1 AND 9223372036854775807 AND payload->>'unit' IN ('tokens','calls','images','requests')) IS TRUE),
 CHECK(kind<>'discount' OR (tenant_id IS NOT NULL AND (payload->>'discount_bps') ~ '^[0-9]+$' AND (payload->>'discount_bps')::integer BETWEEN 0 AND 10000) IS TRUE)
);
CREATE INDEX price_book_v2_terms_identity ON gateway.price_book_v2_terms(provider_id,model_id,model_version,book_version,component,currency,tenant_id,effective_from_ms);

CREATE TABLE gateway.price_book_v2_publications (
 tenant_id text NOT NULL CHECK(tenant_id<>''), provider_id text NOT NULL, model_id text NOT NULL,
 model_version text NOT NULL, book_version text NOT NULL, currency text NOT NULL,
 reference text NOT NULL UNIQUE CHECK(reference ~ '^agentforge\.price_book/v2/publication/[0-9a-f]{64}$'),
 payload jsonb NOT NULL, created_by text NOT NULL CHECK(created_by<>''), created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,provider_id,model_id,model_version,book_version,currency),
 CHECK((payload->>'tenant'=tenant_id AND payload->>'provider'=provider_id AND payload->>'model'=model_id AND payload->>'model_version'=model_version AND payload->>'book_version'=book_version AND payload->>'currency'=currency AND jsonb_typeof(payload->'entries')='array' AND jsonb_array_length(payload->'entries') BETWEEN 1 AND 1024 AND jsonb_typeof(payload->'discounts')='array' AND jsonb_array_length(payload->'discounts')<=1024) IS TRUE)
);
-- Full publication validation avoids an unapproved btree_gist extension. Entries
-- may be reused across immutable publications, but overlaps within one frozen
-- publication are always rejected. Published payloads never read mutable terms.
CREATE FUNCTION gateway.price_book_v2_validate_publication() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $publication$
DECLARE item jsonb; term_kind text;
BEGIN
 FOR term_kind IN SELECT unnest(ARRAY['entry','discount']) LOOP
  FOR item IN SELECT value FROM jsonb_array_elements(NEW.payload -> (CASE WHEN term_kind='entry' THEN 'entries' ELSE 'discounts' END)) LOOP
   IF item->'key'->>'provider' IS DISTINCT FROM NEW.provider_id
      OR item->'key'->>'model' IS DISTINCT FROM NEW.model_id
      OR item->'key'->>'model_version' IS DISTINCT FROM NEW.model_version
      OR item->'key'->>'book_version' IS DISTINCT FROM NEW.book_version
      OR item->'key'->>'currency' IS DISTINCT FROM NEW.currency
      OR (item->>'tenant' IS NOT NULL AND item->>'tenant'<>NEW.tenant_id)
      OR (term_kind='discount' AND item->>'tenant' IS DISTINCT FROM NEW.tenant_id)
      OR NOT EXISTS(SELECT 1 FROM gateway.price_book_v2_terms t WHERE t.kind=term_kind AND t.payload=item) THEN
     RAISE EXCEPTION 'invalid or missing immutable publication term';
   END IF;
  END LOOP;
  IF EXISTS(
    SELECT 1
    FROM jsonb_array_elements(NEW.payload -> (CASE WHEN term_kind='entry' THEN 'entries' ELSE 'discounts' END)) WITH ORDINALITY a(value,position)
    JOIN jsonb_array_elements(NEW.payload -> (CASE WHEN term_kind='entry' THEN 'entries' ELSE 'discounts' END)) WITH ORDINALITY b(value,position) ON a.position<b.position
    WHERE a.value->'key'=b.value->'key' AND (a.value->>'tenant') IS NOT DISTINCT FROM (b.value->>'tenant')
      AND int8range((a.value->'effective'->>'from_ms')::bigint,(a.value->'effective'->>'to_ms')::bigint,'[)') && int8range((b.value->'effective'->>'from_ms')::bigint,(b.value->'effective'->>'to_ms')::bigint,'[)')
  ) THEN RAISE EXCEPTION 'ambiguous overlapping publication terms'; END IF;
 END LOOP;
 RETURN NEW;
END
$publication$;
CREATE TRIGGER price_book_v2_publication_contract BEFORE INSERT ON gateway.price_book_v2_publications FOR EACH ROW EXECUTE FUNCTION gateway.price_book_v2_validate_publication();

CREATE TABLE gateway.price_book_v2_routes (
 tenant_id text NOT NULL, provider_id text NOT NULL, model_id text NOT NULL, currency text NOT NULL,
 model_version text NOT NULL, book_version text NOT NULL, revision bigint NOT NULL CHECK(revision>0),
 updated_by text NOT NULL CHECK(updated_by<>''), updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,provider_id,model_id,currency),
 FOREIGN KEY(tenant_id,provider_id,model_id,model_version,book_version,currency) REFERENCES gateway.price_book_v2_publications(tenant_id,provider_id,model_id,model_version,book_version,currency)
);
CREATE TABLE gateway.price_book_v2_activation_audit (
 tenant_id text NOT NULL, provider_id text NOT NULL, model_id text NOT NULL, currency text NOT NULL,
 revision bigint NOT NULL CHECK(revision>0), publication_reference text NOT NULL REFERENCES gateway.price_book_v2_publications(reference),
 actor_id text NOT NULL CHECK(actor_id<>''), created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,provider_id,model_id,currency,revision)
);
CREATE TABLE gateway.price_book_v2_usage_receipts (
 tenant_id text NOT NULL, request_id text NOT NULL CHECK(request_id<>''), service_id text NOT NULL, request_type text NOT NULL,
 provider_id text NOT NULL, model_id text NOT NULL, snapshot jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,request_id,service_id,request_type),
 CHECK((snapshot->>'tenant_id'=tenant_id AND snapshot->>'provider_id'=provider_id AND snapshot->>'model_id'=model_id AND snapshot->>'currency'='USD' AND snapshot->>'unit'='per_1k_tokens') IS TRUE)
);
CREATE TABLE gateway.price_book_v2_accounting_checkpoints (
 tenant_id text NOT NULL, request_id text NOT NULL, service_id text NOT NULL, request_type text NOT NULL,
 stage text NOT NULL CHECK(stage IN ('quota_counters','daily_aggregates','hourly_aggregates')),
 observation jsonb NOT NULL, updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,request_id,service_id,request_type,stage),
 FOREIGN KEY(tenant_id,request_id,service_id,request_type) REFERENCES gateway.price_book_v2_usage_receipts(tenant_id,request_id,service_id,request_type),
 CHECK((observation->>'tenant_id'=tenant_id AND observation->>'request_id'=request_id AND observation->>'service_id'=service_id AND observation->>'request_type'=request_type AND observation->'metadata'->'pricing_snapshot'->>'price_book_schema'='agentforge.price_book/v2') IS TRUE)
);
CREATE FUNCTION gateway.price_book_v2_checkpoint_receipt() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $checkpoint$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM gateway.price_book_v2_usage_receipts r WHERE r.tenant_id=NEW.tenant_id AND r.request_id=NEW.request_id AND r.service_id=NEW.service_id AND r.request_type=NEW.request_type AND r.snapshot=NEW.observation->'metadata'->'pricing_snapshot') THEN
  RAISE EXCEPTION 'checkpoint receipt does not match frozen authority';
 END IF;
 RETURN NEW;
END
$checkpoint$;
CREATE TRIGGER price_book_v2_checkpoint_bound BEFORE INSERT OR UPDATE ON gateway.price_book_v2_accounting_checkpoints FOR EACH ROW EXECUTE FUNCTION gateway.price_book_v2_checkpoint_receipt();

CREATE TABLE gateway.price_book_v2_import_audit (
 import_id uuid NOT NULL, source_id uuid NOT NULL,
 disposition text NOT NULL CHECK(disposition IN ('attributed','quarantined','inactive')),
 provider_id text, model_version text, book_version text, reason text,
 input_reference text REFERENCES gateway.price_book_v2_terms(reference), output_reference text REFERENCES gateway.price_book_v2_terms(reference),
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(import_id,source_id),
 CHECK(disposition<>'attributed' OR (provider_id IS NOT NULL AND model_version IS NOT NULL AND book_version IS NOT NULL AND input_reference IS NOT NULL AND output_reference IS NOT NULL)),
 CHECK(disposition<>'quarantined' OR reason IS NOT NULL)
);
CREATE FUNCTION gateway.price_book_v2_reject_mutation() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $immutable$
BEGIN RAISE EXCEPTION 'immutable price book authority'; END
$immutable$;
CREATE TRIGGER price_book_v2_terms_immutable BEFORE UPDATE OR DELETE OR TRUNCATE ON gateway.price_book_v2_terms FOR EACH STATEMENT EXECUTE FUNCTION gateway.price_book_v2_reject_mutation();
CREATE TRIGGER price_book_v2_publications_immutable BEFORE UPDATE OR DELETE OR TRUNCATE ON gateway.price_book_v2_publications FOR EACH STATEMENT EXECUTE FUNCTION gateway.price_book_v2_reject_mutation();
CREATE TRIGGER price_book_v2_activation_audit_immutable BEFORE UPDATE OR DELETE OR TRUNCATE ON gateway.price_book_v2_activation_audit FOR EACH STATEMENT EXECUTE FUNCTION gateway.price_book_v2_reject_mutation();
CREATE TRIGGER price_book_v2_usage_receipts_immutable BEFORE UPDATE OR DELETE OR TRUNCATE ON gateway.price_book_v2_usage_receipts FOR EACH STATEMENT EXECUTE FUNCTION gateway.price_book_v2_reject_mutation();
CREATE TRIGGER price_book_v2_import_audit_immutable BEFORE UPDATE OR DELETE OR TRUNCATE ON gateway.price_book_v2_import_audit FOR EACH STATEMENT EXECUTE FUNCTION gateway.price_book_v2_reject_mutation();
REVOKE ALL ON FUNCTION gateway.price_book_v2_validate_publication() FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.price_book_v2_checkpoint_receipt() FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.price_book_v2_reject_mutation() FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.price_book_v2_terms,gateway.price_book_v2_publications,gateway.price_book_v2_routes,gateway.price_book_v2_activation_audit,gateway.price_book_v2_usage_receipts,gateway.price_book_v2_accounting_checkpoints,gateway.price_book_v2_import_audit TO %I',principal);
 EXECUTE format('GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I',principal);
 EXECUTE format('GRANT UPDATE(observation,updated_at) ON gateway.price_book_v2_accounting_checkpoints TO %I',principal);
END
$grants$;
-- END FIXED SOURCE price-book-v2

-- BEGIN FIXED SOURCE runtime-cache-settlement-v2: database/proposals/runtime-cache-settlement-v2.sql
-- Depends on price-book v2; adds the Gateway-only SECURITY DEFINER settlement boundary.
-- Frozen source SHA-256: 64e857cd925c3f0c6dfc566ee78cf6bac8bd4dc67f900879db03945a985a84a7
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
-- END FIXED SOURCE runtime-cache-settlement-v2

-- BEGIN FIXED SOURCE runtime-capability-witness-store: docs/design/proposals/runtime-capability-witness-store.sql
-- Adds managed and SDK attenuation-v3 witnesses with owner-verified snapshot binding and no Worker/Knowledge/PUBLIC access.
-- Frozen source SHA-256: 5e396ac9ebdd4d845da92ce0a47b993a849547cbd40a5474825394c64fbcc0fe
-- INACTIVE PROPOSAL ONLY. Native authority must assign a successor and grants,
-- verify frozen epoch compatibility, update admission and activate separately.
-- Never source this file directly into an existing developer/production DB.
-- Required principle: Worker/Knowledge/public roles cannot read attenuation seeds.
CREATE TABLE assistant.assistant_runtime_capability_witnesses (
    run_id UUID PRIMARY KEY REFERENCES assistant.assistant_runs(run_id) ON DELETE CASCADE,
    tenant_id VARCHAR NOT NULL,
    user_id VARCHAR NOT NULL,
    session_id VARCHAR NOT NULL,
    snapshot_id UUID NOT NULL,
    managed_snapshot_id UUID GENERATED ALWAYS AS (CASE WHEN token#>>'{root,purpose}'='capability-ceiling' THEN snapshot_id END) STORED REFERENCES assistant.assistant_runtime_snapshots(snapshot_id) ON DELETE CASCADE,
    sdk_snapshot_id UUID GENERATED ALWAYS AS (CASE WHEN token#>>'{root,purpose}'='sdk-capability-ceiling' THEN snapshot_id END) STORED REFERENCES assistant.assistant_sdk_runs(snapshot_id) ON DELETE CASCADE,
    token JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (jsonb_typeof(token)='object'),
    CHECK ((token#>>'{root,purpose}' IN ('capability-ceiling','sdk-capability-ceiling')) IS TRUE),
    CHECK (octet_length(token::text) <= 262144),
    CHECK ((token->>'schema_version'='capability-attenuation/v3') IS TRUE),
    CHECK ((token#>>'{proof,kind}'='open') IS TRUE),
    CHECK ((token#>>'{root,claims,run_id}'=run_id::text) IS TRUE),
    CHECK ((token#>>'{root,claims,snapshot_id}'=snapshot_id::text) IS TRUE),
    CHECK ((token#>>'{root,claims,tenant_id}'=tenant_id) IS TRUE),
    CHECK ((token#>>'{root,claims,user_id}'=user_id) IS TRUE),
    CHECK ((token#>>'{root,claims,session_id}'=session_id) IS TRUE)
);
REVOKE ALL ON assistant.assistant_runtime_capability_witnesses FROM PUBLIC;
-- Deployment-specific role names MUST be resolved by native authority, never
-- pasted as grants to PUBLIC or inherited groups. Requirements, not active SQL:
-- Gateway: INSERT, SELECT; Runtime: SELECT; capability_worker/Knowledge: NONE.
-- Retention/deletion is a separate scoped authority operation, never a consumer
-- UPDATE/DELETE privilege. Add actual-role positive/negative/frozen verification.

-- Cross-table scope, immutable public envelope and actual engine must agree.
-- Both branches retain real snapshot FKs; SDK uses assistant_sdk_runs, which
-- issue_sdk_execution already links to the shared assistant_runs authority.
CREATE FUNCTION assistant.assert_capability_witness_binding() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF NEW.token#>>'{root,purpose}'='capability-ceiling' THEN
        IF NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshots s JOIN assistant_runs r ON r.run_id=s.run_id
            WHERE s.snapshot_id=NEW.snapshot_id AND s.run_id=NEW.run_id AND r.runtime_snapshot_id=s.snapshot_id
            AND r.engine='agent_runtime' AND s.tenant_id=NEW.tenant_id AND s.user_id=NEW.user_id AND s.session_id=NEW.session_id
            AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id AND r.session_id=NEW.session_id
            AND NEW.token->'root'=s.snapshot->'capability_ceiling'
            AND NEW.token#>>'{root,claims,runtime_thread_id}'=s.runtime_thread_id::text
            AND NEW.token#>>'{root,claims,capability_revision}'=s.capability_revision::text) THEN
            RAISE EXCEPTION 'CAPABILITY_WITNESS_BINDING_INVALID' USING ERRCODE='42501';
        END IF;
    ELSE
        IF NOT EXISTS (SELECT 1 FROM assistant_sdk_runs s JOIN assistant_runs r ON r.run_id=s.run_id
            WHERE s.snapshot_id=NEW.snapshot_id AND s.run_id=NEW.run_id AND r.engine=s.engine
            AND s.engine IN ('openai_agents','claude_agent_sdk')
            AND s.tenant_id=NEW.tenant_id AND s.user_id=NEW.user_id AND s.session_id=NEW.session_id
            AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id AND r.session_id=NEW.session_id
            AND s.owner_id IS NOT NULL AND s.fence>0
            AND NEW.token->'root'=s.snapshot->'capability_ceiling'
            AND NEW.token#>>'{root,claims,runtime_thread_id}'=''
            AND NEW.token#>>'{root,claims,capability_revision}'=s.capability_revision::text) THEN
            RAISE EXCEPTION 'CAPABILITY_WITNESS_BINDING_INVALID' USING ERRCODE='42501';
        END IF;
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION assistant.assert_capability_witness_binding() FROM PUBLIC;
CREATE TRIGGER capability_witness_binding BEFORE INSERT OR UPDATE ON assistant.assistant_runtime_capability_witnesses
FOR EACH ROW EXECUTE FUNCTION assistant.assert_capability_witness_binding();
-- END FIXED SOURCE runtime-capability-witness-store

-- Keep caller TEMP objects behind trusted schemas for this SECURITY DEFINER trigger.
ALTER FUNCTION assistant.assert_capability_witness_binding() SET search_path=pg_catalog,assistant,pg_temp;
DO $witness_grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
    IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN
        RAISE EXCEPTION 'invalid canonical role prefix';
    END IF;
    EXECUTE format(
        'GRANT INSERT,SELECT ON assistant.assistant_runtime_capability_witnesses TO %I',
        prefix||'gateway'
    );
    EXECUTE format(
        'GRANT SELECT ON assistant.assistant_runtime_capability_witnesses TO %I',
        prefix||'runtime'
    );
END
$witness_grants$;

-- BEGIN FIXED SOURCE local-node-owner-emergency-stop-pop4: docs/design/local-node-owner-emergency-stop.sql.proposal
-- Adds owner-bound emergency stop and PoP4 request proof records.
-- Frozen source SHA-256: e2f47ab2fbd7f5e21654a5e18418275b0a4a12ad4f82c33f63bcc731362d83ff
-- UNACTIVATED, UNNUMBERED successor proposal. No direct psql/startup execution.
-- Canonical authority must measure complete predecessor/post fingerprints and
-- activate this with its own source lock; current 023/024 are immutable.
DO $owner$
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
 OR current_user <> (SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname='gateway')
 OR EXISTS(SELECT 1 FROM pg_roles WHERE rolname=current_user AND
   (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)) THEN
  RAISE EXCEPTION 'canonical unprivileged NOLOGIN owner required';
 END IF;
END $owner$;
CREATE TABLE gateway.local_node_control_commands (
 command_id uuid PRIMARY KEY,
 tenant_id varchar(128) NOT NULL,
 user_id varchar(128) NOT NULL,
 device_id varchar(255) NOT NULL,
 idempotency_key uuid NOT NULL,
 kind text NOT NULL CHECK(kind='emergency_stop'),
 intent_sha256 char(64) NOT NULL CHECK(intent_sha256 ~ '^[0-9a-f]{64}$'),
 nonce_seed char(64) NOT NULL CHECK(nonce_seed ~ '^[0-9a-f]{64}$'),
 requested_at timestamptz NOT NULL DEFAULT now(),
 acknowledged_at timestamptz,
 acknowledged_channel_id varchar(255) REFERENCES gateway.local_node_channels(channel_id) ON DELETE RESTRICT,
 UNIQUE(tenant_id,user_id,idempotency_key),
 FOREIGN KEY(device_id,tenant_id,user_id) REFERENCES gateway.local_node_devices(device_id,tenant_id,user_id) ON DELETE RESTRICT,
 CHECK((acknowledged_at IS NULL)=(acknowledged_channel_id IS NULL))
);
CREATE INDEX local_node_control_commands_pending ON gateway.local_node_control_commands(tenant_id,user_id,device_id,requested_at) WHERE acknowledged_at IS NULL;
CREATE TABLE gateway.local_node_control_deliveries (
 delivery_id varchar(512) PRIMARY KEY,
 command_id uuid NOT NULL REFERENCES gateway.local_node_control_commands(command_id) ON DELETE RESTRICT,
 nonce char(64) NOT NULL CHECK(nonce ~ '^[0-9a-f]{64}$'),
 issued_at double precision NOT NULL CHECK(issued_at>0 AND issued_at<'Infinity'::float8),
 expires_at double precision NOT NULL CHECK(expires_at>issued_at AND expires_at-issued_at<=600),
 recorded_at timestamptz NOT NULL DEFAULT now()
);
REVOKE ALL ON gateway.local_node_control_commands,gateway.local_node_control_deliveries FROM PUBLIC;
DO $grant$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid role prefix'; END IF;
 EXECUTE format('GRANT SELECT,INSERT ON gateway.local_node_control_commands TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(acknowledged_at,acknowledged_channel_id) ON gateway.local_node_control_commands TO %I',prefix||'gateway');
 EXECUTE format('GRANT SELECT,INSERT ON gateway.local_node_control_deliveries TO %I',prefix||'gateway');
END $grant$;

-- Request PoP v1, activated atomically with the owner-control tables above.
-- Keys are immutable, registered ONLY in the owner-bound signed pairing tx.
CREATE TABLE gateway.local_node_request_keys (
 channel_id varchar(255) PRIMARY KEY REFERENCES gateway.local_node_channels(channel_id) ON DELETE RESTRICT,
 public_key varchar(128) NOT NULL CHECK(public_key ~ '^[A-Za-z0-9_-]{43}$'),
 key_class text NOT NULL CHECK(key_class='os-vault-software-ed25519'),
 registered_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE gateway.local_node_request_nonces (
 credential_sha256 char(64) PRIMARY KEY REFERENCES gateway.local_node_device_credentials(credential_sha256) ON DELETE RESTRICT,
 nonce varchar(128) NOT NULL CHECK(nonce ~ '^[A-Za-z0-9_-]{43}$'),
 issued_at bigint NOT NULL CHECK(issued_at>0),
 expires_at bigint NOT NULL CHECK(expires_at>issued_at AND expires_at-issued_at<=60),
 consumed_at timestamptz
);
REVOKE ALL ON gateway.local_node_request_keys,gateway.local_node_request_nonces FROM PUBLIC;
DO $proof_grant$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid role prefix'; END IF;
 EXECUTE format('GRANT SELECT,INSERT ON gateway.local_node_request_keys TO %I',prefix||'gateway');
 EXECUTE format('GRANT SELECT,INSERT ON gateway.local_node_request_nonces TO %I',prefix||'gateway');
 EXECUTE format('GRANT UPDATE(nonce,issued_at,expires_at,consumed_at) ON gateway.local_node_request_nonces TO %I',prefix||'gateway');
END $proof_grant$;
-- END FIXED SOURCE local-node-owner-emergency-stop-pop4

-- BEGIN FIXED SOURCE knowledge-vector-mutation-jobs: database/proposals/knowledge-vector-mutation-jobs.sql
-- Adds the API-to-worker durable vector mutation queue with least-privilege grants.
-- Frozen source SHA-256: 314b56d76e39ddc286ce63d0267bdda8d4a1b6411d1d6c39f6244b8c10dd2408
-- Unnumbered proposal: primary authority must assign/grant/verify canonically.
-- API role inserts/reads; worker role additionally updates. No Qdrant key or
-- bearer token is persisted. Dataset is retained after deletion (tombstone).
DO $owner_pre$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user<>(SELECT pg_catalog.pg_get_userbyid(nspowner)
                         FROM pg_catalog.pg_namespace WHERE nspname='knowledge')
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=current_user AND
                 (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR
                  rolreplication OR rolbypassrls))
       OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=prefix||'knowledge_api')
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=prefix||'knowledge_worker') THEN
        RAISE EXCEPTION 'canonical NOLOGIN Knowledge owner and roles required';
    END IF;
END $owner_pre$;

CREATE TABLE knowledge.vector_mutation_jobs (
    job_id TEXT PRIMARY KEY CHECK (job_id ~ '^[0-9a-f]{64}$'),
    tenant_id VARCHAR(255) NOT NULL,
    dataset_id VARCHAR(255) NOT NULL,
    requested_by VARCHAR(255) NOT NULL,
    expected_generation TEXT NOT NULL,
    expected_revision BIGINT,
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','running','succeeded','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    result JSONB,
    error_class TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (length(tenant_id)>0 AND length(dataset_id)>0 AND length(requested_by)>0)
);
CREATE INDEX vector_mutation_jobs_dispatch ON knowledge.vector_mutation_jobs(created_at,job_id) WHERE state IN ('pending','running');
CREATE INDEX vector_mutation_jobs_tenant ON knowledge.vector_mutation_jobs(tenant_id,dataset_id,requested_by);

REVOKE ALL ON knowledge.vector_mutation_jobs FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
    IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN
        RAISE EXCEPTION 'invalid canonical Knowledge role prefix';
    END IF;
    EXECUTE format('GRANT SELECT ON knowledge.vector_mutation_jobs TO %I',prefix||'knowledge_api');
    EXECUTE format(
        'GRANT INSERT(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_api'
    );
    EXECUTE format(
        'GRANT UPDATE(state,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_api'
    );
    EXECUTE format('GRANT SELECT ON knowledge.vector_mutation_jobs TO %I',prefix||'knowledge_worker');
    EXECUTE format(
        'GRANT UPDATE(state,attempts,result,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_worker'
    );
END $grants$;

DO $owner_post$
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user<>(SELECT pg_catalog.pg_get_userbyid(nspowner)
                         FROM pg_catalog.pg_namespace WHERE nspname='knowledge')
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=current_user AND
                 (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR
                  rolreplication OR rolbypassrls))
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_class c
                     JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                     WHERE n.nspname='knowledge' AND c.relname='vector_mutation_jobs'
                       AND pg_catalog.pg_get_userbyid(c.relowner)=current_user)
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_class c
                 JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                 CROSS JOIN LATERAL pg_catalog.aclexplode(
                     COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a
                 WHERE n.nspname='knowledge' AND c.relname='vector_mutation_jobs'
                   AND a.grantee=0) THEN
        RAISE EXCEPTION 'vector mutation job owner or public ACL changed';
    END IF;
END $owner_post$;
-- END FIXED SOURCE knowledge-vector-mutation-jobs
