pub(super) const APPROVED_SERVICE_ROLE_GRANTS: &[(&str, &str)] = &[
    (
        "runtime",
        "GRANT SELECT ON assistant.assistant_runtime_thread_owners TO %I",
    ),
    ("gateway", "GRANT SELECT ON gateway.sdk_model_calls TO %I"),
    (
        "knowledge_worker",
        "GRANT INSERT,SELECT ON knowledge.pipeline_work TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT ON gateway.authorization_decisions TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT ON gateway.authorization_resource_decisions TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT,SELECT ON gateway.gateway_a2a_contexts TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT,SELECT ON gateway.gateway_a2a_tasks TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT,SELECT ON gateway.gateway_a2a_outbound_effects TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(created_at) ON gateway.gateway_a2a_contexts TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(state,dispatched_at,provider_request_id,error_code,observation) ON gateway.sdk_model_calls TO %I",
    ),
    (
        "knowledge_worker",
        "GRANT UPDATE(job_id,phase,status,execution_id,checkpoints,owner_token,lease_expires_at,fence,updated_at) ON knowledge.pipeline_work TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(effect_state,run_id,thread_id,updated_at) ON gateway.gateway_a2a_tasks TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(effect_state,response,updated_at) ON gateway.gateway_a2a_outbound_effects TO %I",
    ),
    (
        "runtime",
        "GRANT EXECUTE ON FUNCTION assistant.claim_runtime_thread_owner(uuid, uuid, text) TO %I",
    ),
    (
        "runtime",
        "GRANT EXECUTE ON FUNCTION assistant.assert_runtime_thread_owner_for_run(uuid, uuid, text) TO %I",
    ),
    (
        "runtime",
        "GRANT EXECUTE ON FUNCTION assistant.heartbeat_runtime_thread_owner(uuid, uuid) TO %I",
    ),
    (
        "gateway",
        "GRANT EXECUTE ON FUNCTION gateway.reserve_sdk_model_call(text, text, text, text, uuid, jsonb, jsonb, jsonb, jsonb, bigint, bigint, bigint) TO %I",
    ),
    (
        "gateway",
        "GRANT EXECUTE ON FUNCTION gateway.settle_sdk_model_call(uuid, text, text, bigint, bigint, bigint, bigint, jsonb, jsonb) TO %I",
    ),
    (
        "gateway",
        "REVOKE ALL ON gateway.local_node_identity_policy,gateway.local_node_attestation_verifiers,gateway.local_node_hardware_attestations FROM %I",
    ),
    (
        "gateway",
        "GRANT SELECT ON gateway.local_node_identity_policy,gateway.local_node_attestation_verifiers,gateway.local_node_hardware_attestations TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(remote_task_id,cancel_state) ON gateway.gateway_a2a_outbound_effects TO %I",
    ),
];
