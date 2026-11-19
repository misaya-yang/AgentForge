-- PROPOSAL ONLY. Independent successor to measured price-book/schema 026.
-- Apply before enabling registered request-meter models; never modify 026.
BEGIN;
DO $owner$
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR current_user<>(SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname='gateway') THEN RAISE EXCEPTION 'canonical Gateway owner required'; END IF;
END $owner$;
-- Preserve every original price component wire value; append request explicitly.
ALTER TABLE gateway.price_book_v2_terms DROP CONSTRAINT price_book_v2_terms_component_check;
ALTER TABLE gateway.price_book_v2_terms ADD CONSTRAINT price_book_v2_terms_component_check CHECK(component IN ('input','cached_input','cache_write','output','reasoning','tool_call','image','batch','request'));
ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_check;
ALTER TABLE gateway.price_book_v2_usage_receipts ADD CONSTRAINT price_book_v2_usage_receipts_check CHECK((snapshot->>'tenant_id'=tenant_id AND snapshot->>'provider_id'=provider_id AND snapshot->>'model_id'=model_id AND snapshot->>'currency'='USD' AND (snapshot->>'unit'='per_1k_tokens' OR (snapshot->>'unit'='per_request' AND snapshot->>'pricing_meter'='requests' AND snapshot->'price_book_request'->'entry'->'key'->>'component'='request' AND snapshot->'price_book_request'->'entry'->>'unit'='requests'))) IS TRUE);
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
) IS TRUE);
-- No new writable table or broad privilege. Generated columns require no INSERT
-- grants. Existing authorized Gateway ledger readers retain aggregate cost and
-- invocation counts; request physical token values are nullable typed facts.
-- An ordinary legacy SUM(tokens) remains the sum of applicable known token facts.
-- Billing summary adds metering annotation when request facts exist; cache ratios
-- exclude N/A request rows instead of decoding them as zero or cache unknown.
COMMIT;
