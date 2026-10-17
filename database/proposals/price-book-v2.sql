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
