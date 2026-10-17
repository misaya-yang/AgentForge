-- U-GW-01 PROPOSAL ONLY. Unnumbered, unregistered, NOT an admitted migration.
-- Requires authority review, successor allocation (023 is reserved), privileges,
-- btree_gist availability and frozen-schema verification before application.
-- Does not mutate model_pricing, llm_models, historical receipts or ledgers.
BEGIN;
CREATE EXTENSION IF NOT EXISTS btree_gist;
CREATE TABLE price_book_v2_terms (
    reference text PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('entry','discount')),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload)='object'),
    tenant_id text GENERATED ALWAYS AS (payload->>'tenant') STORED,
    book_version text GENERATED ALWAYS AS (payload->'key'->>'book_version') STORED NOT NULL,
    provider_id text GENERATED ALWAYS AS (payload->'key'->>'provider') STORED NOT NULL,
    model_id text GENERATED ALWAYS AS (payload->'key'->>'model') STORED NOT NULL,
    model_version text GENERATED ALWAYS AS (payload->'key'->>'model_version') STORED NOT NULL,
    component text GENERATED ALWAYS AS (payload->'key'->>'component') STORED NOT NULL,
    currency text GENERATED ALWAYS AS (payload->'key'->>'currency') STORED NOT NULL,
    effective_from_ms bigint GENERATED ALWAYS AS ((payload->'effective'->>'from_ms')::bigint) STORED NOT NULL,
    effective_to_ms bigint GENERATED ALWAYS AS ((payload->'effective'->>'to_ms')::bigint) STORED,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (reference LIKE 'agentforge.price_book/v2/' || kind || '/%'),
    CHECK (tenant_id IS NULL OR length(trim(tenant_id)) > 0),
    CHECK (length(trim(book_version))>0 AND length(trim(provider_id))>0 AND length(trim(model_id))>0 AND length(trim(model_version))>0),
    CHECK (currency ~ '^[A-Z]{3}$'),
    CHECK (component IN ('input','cached_input','cache_write','output','reasoning','tool_call','image','batch')),
    CHECK (effective_to_ms IS NULL OR effective_to_ms>effective_from_ms),
    CHECK (kind<>'discount' OR (tenant_id IS NOT NULL AND ((payload->>'discount_bps')::integer BETWEEN 0 AND 10000) IS TRUE)),
    CHECK (kind<>'entry' OR ((payload->>'rate_nanos')::numeric BETWEEN 0 AND 9223372036854775807
        AND (payload->>'units_per_rate')::numeric BETWEEN 1 AND 9223372036854775807
        AND payload->>'unit' IN ('tokens','calls','images','requests')) IS TRUE),
    EXCLUDE USING gist (
        kind WITH =, (coalesce(tenant_id,'')) WITH =,
        book_version WITH =, provider_id WITH =, model_id WITH =, model_version WITH =,
        component WITH =, currency WITH =,
        (int8range(effective_from_ms,effective_to_ms,'[)')) WITH &&
    )
);
CREATE FUNCTION price_book_v2_reject_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'price_book_v2_terms_are_immutable';
END;
$$;
CREATE TRIGGER price_book_v2_immutable
BEFORE UPDATE OR DELETE OR TRUNCATE ON price_book_v2_terms
FOR EACH STATEMENT EXECUTE FUNCTION price_book_v2_reject_mutation();
-- Audit rows contain the source identity, reviewed attribution and quarantine
-- reason, never secrets. An operator must persist the plan_legacy_import report
-- with the admitted migration receipt. No provider is guessed in SQL.
CREATE TABLE price_book_v2_import_audit (
    import_id uuid NOT NULL,
    source_id uuid NOT NULL,
    disposition text NOT NULL CHECK (disposition IN ('attributed','quarantined','inactive')),
    provider_id text,
    model_version text,
    reason text,
    input_reference text REFERENCES price_book_v2_terms(reference),
    output_reference text REFERENCES price_book_v2_terms(reference),
    PRIMARY KEY(import_id,source_id),
    CHECK (disposition<>'attributed' OR (provider_id IS NOT NULL AND model_version IS NOT NULL
        AND input_reference IS NOT NULL AND output_reference IS NOT NULL)),
    CHECK (disposition<>'quarantined' OR reason IS NOT NULL)
);
CREATE TRIGGER price_book_v2_import_audit_immutable
BEFORE UPDATE OR DELETE OR TRUNCATE ON price_book_v2_import_audit
FOR EACH STATEMENT EXECUTE FUNCTION price_book_v2_reject_mutation();
COMMENT ON TABLE price_book_v2_terms IS 'Opt-in immutable v2 terms; legacy billing remains unchanged until host activation';
COMMIT;
