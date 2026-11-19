-- Non-unique additive lookup acceleration for SDK observation phases only.
-- Existing duplicates remain evidence and are rejected by the journal writer;
-- this migration neither deletes history nor imposes an unreviewed uniqueness rule.
-- The journal SELECT uses literal predicates so generic prepared plans can use it.
CREATE INDEX sdk_usage_audit_lookup
    ON gateway.audit_logs (tenant_id, user_id, resource_id, action, id)
    WHERE event_type = 'sdk_usage_observation' AND resource_type = 'sdk_request';
COMMENT ON INDEX gateway.sdk_usage_audit_lookup IS
    'Scoped SDK audit observation lookup and LIMIT 2 duplicate detection; not a charge ledger or a uniqueness guarantee.';
