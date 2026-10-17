//! Exact Gateway-only templates reviewed against measured027 ACL additions.
//! This does not permit arbitrary dynamic SQL, roles, objects or privileges.
pub(super) const APPROVED_027_ROLE_GRANTS: &[(&str, &str)] = &[
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.dispatch_token_reservations TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(state,candidate_receipt_id,settlement_candidate,settlement_observation,updated_at) ON gateway.dispatch_token_reservations TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.enterprise_identity_links,gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_scim_members,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes,gateway.enterprise_session_assurance TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE ON gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I",
    ),
    (
        "gateway",
        "GRANT DELETE ON gateway.enterprise_scim_members,gateway.enterprise_scim_groups,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT ON gateway.enterprise_scim_role_map,gateway.enterprise_tenant_policy TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.routing_policy_versions,gateway.routing_decisions TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT,UPDATE ON gateway.routing_policy_active TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT ON gateway.knowledge_model_calls TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(state,dispatched_at,provider_request_id,error_code,observation) ON gateway.knowledge_model_calls TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT ON gateway.knowledge_model_workload_nonces TO %I",
    ),
    (
        "gateway",
        "GRANT EXECUTE ON FUNCTION gateway.reserve_knowledge_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint) TO %I",
    ),
    (
        "gateway",
        "GRANT EXECUTE ON FUNCTION gateway.settle_knowledge_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb) TO %I",
    ),
];
