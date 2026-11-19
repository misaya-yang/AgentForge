-- R3 AG-06/07: an explicitly public Agent publication has a durable deadline.
-- Existing publications remain nullable for forward compatibility; new public
-- releases are validated by the Gateway before writing this field.
ALTER TABLE gateway.agent_publications
    ADD COLUMN expires_at TIMESTAMPTZ;

COMMENT ON COLUMN gateway.agent_publications.expires_at IS
    'Gateway-authoritative public audience deadline; NULL preserves legacy/internal publications';
