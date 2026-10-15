-- Additive epoch: common SDK lifecycle admission without granting any tools.
-- CREATE OR REPLACE preserves the existing Gateway-only EXECUTE ACL and signature.
CREATE OR REPLACE FUNCTION assistant.issue_sdk_execution(p_run uuid,p_tenant varchar,p_user varchar,p_session varchar,p_service varchar,p_request varchar,p_engine varchar,p_request_hash char(64),p_snapshot uuid,p_revision bigint,p_payload jsonb,p_hash char(64),p_owner uuid)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,gateway AS $$
DECLARE empty_selection boolean;
BEGIN
    -- This narrow function is Gateway-only. Public fields never choose the owner or snapshot.
    PERFORM 1 FROM gateway.sdk_session_barriers WHERE tenant_id=p_tenant AND user_id=p_user AND session_id=p_session AND NOT deleting FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_SESSION_UNAVAILABLE' USING ERRCODE='42501'; END IF;
    PERFORM 1 FROM assistant.sessions s WHERE s.tenant_id=p_tenant AND s.user_id=p_user AND s.session_id=p_session AND s.service_id=p_service
        AND s.status<>'deleted' AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp()) FOR SHARE OF s;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_SESSION_UNAVAILABLE' USING ERRCODE='42501'; END IF;
    IF jsonb_typeof(p_payload#>'{readonly_capabilities,capability_allowlist}') IS DISTINCT FROM 'array'
        OR jsonb_array_length(p_payload#>'{readonly_capabilities,capability_allowlist}') NOT BETWEEN 0 AND 16
        OR p_engine NOT IN ('openai_agents','claude_agent_sdk') THEN
        RAISE EXCEPTION 'SDK_SNAPSHOT_INVALID' USING ERRCODE='42501';
    END IF;
    empty_selection := jsonb_array_length(p_payload#>'{readonly_capabilities,capability_allowlist}')=0;
    IF empty_selection AND (
        jsonb_typeof(p_payload->'readonly_capabilities') IS DISTINCT FROM 'object'
        OR p_payload#>'{readonly_capabilities,items}' IS DISTINCT FROM '[]'::jsonb
        OR p_payload#>'{readonly_capabilities,tools}' IS DISTINCT FROM '[]'::jsonb
        OR p_payload#>'{readonly_capabilities,mcp}' IS DISTINCT FROM '[]'::jsonb
        OR p_payload#>'{readonly_capabilities,deferred}' IS DISTINCT FROM '[]'::jsonb
        OR jsonb_typeof(p_payload->'sdk_tools') IS DISTINCT FROM 'object'
        OR ((p_payload->'sdk_tools') ? 'capability_ids'
            AND p_payload#>'{sdk_tools,capability_ids}' IS DISTINCT FROM '[]'::jsonb)
    ) THEN
        RAISE EXCEPTION 'SDK_EMPTY_SELECTION_INVALID' USING ERRCODE='42501';
    END IF;
    INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id,engine,status) VALUES(p_run,p_tenant,p_user,p_session,p_engine,'running');
    INSERT INTO assistant_sdk_runs(run_id,tenant_id,user_id,session_id,service_id,request_id,engine,request_sha256,snapshot_id,capability_revision,snapshot,snapshot_sha256,owner_id,fence,lease_until,expires_at)
    VALUES(p_run,p_tenant,p_user,p_session,p_service,p_request,p_engine,p_request_hash,p_snapshot,p_revision,p_payload,p_hash,p_owner,1,clock_timestamp()+interval '30 seconds',clock_timestamp()+CASE WHEN empty_selection THEN interval '5 minutes' ELSE interval '30 minutes' END);
    PERFORM assert_sdk_execution(p_run,p_tenant,p_user,p_session,p_owner,1,TRUE);
END $$;
