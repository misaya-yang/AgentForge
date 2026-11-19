-- Migration: 058_usage_request_identity_idempotency.sql
-- Goal: make durable usage accounting idempotent by request identity.

BEGIN;

CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_records_request_identity
ON usage_records (
    tenant_id,
    COALESCE(request_id, ''),
    COALESCE(service_id, ''),
    COALESCE(request_type, '')
)
WHERE request_id IS NOT NULL AND request_id <> '';

COMMIT;
