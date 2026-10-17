-- PROPOSAL ONLY. Contract phase after measured expansion-only successor 027 v2.
-- Do not apply until both new checks are installed and validated by 027.
DO $successor_028_precondition$
BEGIN
 IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_check' AND convalidated AND position('cached_input' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('request' in pg_catalog.pg_get_constraintdef(oid))=0)
    OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_request_v2_check' AND convalidated AND position('cached_input' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('request' in pg_catalog.pg_get_constraintdef(oid))>0)
    OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_check' AND convalidated AND position('per_1k_tokens' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))=0)
    OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_request_v2_check' AND convalidated AND position('per_1k_tokens' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))>0)
 THEN RAISE EXCEPTION '027 v2 price-book expansion checks are not validated'; END IF;
END;
$successor_028_precondition$;

ALTER TABLE gateway.price_book_v2_terms DROP CONSTRAINT price_book_v2_terms_component_check;
ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_check;
