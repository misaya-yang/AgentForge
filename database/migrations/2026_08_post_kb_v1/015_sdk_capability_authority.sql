-- Genuine SDK runs share approvals/executions, never Codex thread lineage.
-- Existing Runtime-only tables and CHECK/FK constraints are deliberately unchanged.
CREATE TABLE assistant.assistant_sdk_runs (
    run_id uuid PRIMARY KEY REFERENCES assistant.assistant_runs(run_id),
    tenant_id varchar(64) NOT NULL, user_id varchar(64) NOT NULL,
    session_id varchar(100) NOT NULL, service_id varchar(255) NOT NULL,
    request_id varchar(255) NOT NULL,
    engine varchar(32) NOT NULL CHECK (engine IN ('openai_agents','claude_agent_sdk')),
    request_sha256 char(64) NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    snapshot_id uuid UNIQUE NOT NULL,
    capability_revision bigint NOT NULL CHECK (capability_revision > 0),
    snapshot jsonb NOT NULL CHECK (jsonb_typeof(snapshot)='object'),
    snapshot_sha256 char(64) NOT NULL CHECK (snapshot_sha256 ~ '^[0-9a-f]{64}$'),
    owner_id uuid NOT NULL, fence bigint NOT NULL CHECK (fence > 0),
    lease_until timestamptz NOT NULL, expires_at timestamptz NOT NULL,
    revoked_at timestamptz, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (tenant_id,user_id,service_id,request_id),
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '30 minutes'),
    CHECK (lease_until <= expires_at)
);
CREATE INDEX sdk_run_scope ON assistant.assistant_sdk_runs(tenant_id,user_id,session_id);
CREATE TABLE assistant.assistant_sdk_tool_calls (
    run_id uuid NOT NULL REFERENCES assistant.assistant_sdk_runs(run_id),
    tool_call_id varchar(160) NOT NULL CHECK (length(tool_call_id) > 0),
    tool_name varchar(160) NOT NULL, namespace text,
    arguments jsonb NOT NULL CHECK (jsonb_typeof(arguments)='object'),
    arguments_sha256 char(64) NOT NULL CHECK (arguments_sha256 ~ '^[0-9a-f]{64}$'),
    approval_id uuid REFERENCES assistant.assistant_tool_approvals(approval_id),
    execution_id uuid REFERENCES assistant.assistant_capability_executions(execution_id),
    dispatch_started_at timestamptz,
    response jsonb CHECK (response IS NULL OR jsonb_typeof(response)='object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(), completed_at timestamptz,
    PRIMARY KEY(run_id,tool_call_id),
    CHECK ((response IS NULL) = (completed_at IS NULL))
);
CREATE FUNCTION assistant.guard_sdk_run() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF TG_OP='INSERT' THEN
        PERFORM 1 FROM assistant_runs r WHERE r.run_id=NEW.run_id AND r.engine=NEW.engine
            AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id AND r.session_id=NEW.session_id
            AND r.status='running' AND r.harness_thread_id IS NULL AND r.harness_turn_id IS NULL
            AND r.runtime_snapshot_id IS NULL AND r.kernel_revision IS NULL AND r.capability_revision IS NULL;
        IF NOT FOUND THEN RAISE EXCEPTION 'SDK_RUN_SCOPE_INVALID' USING ERRCODE='42501'; END IF;
    ELSIF (to_jsonb(NEW)-ARRAY['lease_until','revoked_at']) IS DISTINCT FROM (to_jsonb(OLD)-ARRAY['lease_until','revoked_at'])
        OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at)
        OR (NEW.lease_until>OLD.lease_until AND (OLD.lease_until<=clock_timestamp() OR OLD.revoked_at IS NOT NULL)) THEN
        RAISE EXCEPTION 'SDK_RUN_AUTHORITY_IMMUTABLE' USING ERRCODE='42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sdk_run_immutable BEFORE INSERT OR UPDATE ON assistant.assistant_sdk_runs
    FOR EACH ROW EXECUTE FUNCTION assistant.guard_sdk_run();
CREATE FUNCTION assistant.guard_sdk_call() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF (NEW.run_id,NEW.tool_call_id,NEW.tool_name,NEW.namespace,NEW.arguments,NEW.arguments_sha256,NEW.created_at)
        IS DISTINCT FROM (OLD.run_id,OLD.tool_call_id,OLD.tool_name,OLD.namespace,OLD.arguments,OLD.arguments_sha256,OLD.created_at)
        OR (OLD.approval_id IS NOT NULL AND NEW.approval_id IS DISTINCT FROM OLD.approval_id)
        OR (OLD.execution_id IS NOT NULL AND NEW.execution_id IS DISTINCT FROM OLD.execution_id)
        OR (OLD.dispatch_started_at IS NOT NULL AND NEW.dispatch_started_at IS DISTINCT FROM OLD.dispatch_started_at)
        OR (OLD.response IS NOT NULL AND (NEW.response,NEW.completed_at) IS DISTINCT FROM (OLD.response,OLD.completed_at)) THEN
        RAISE EXCEPTION 'SDK_CALL_IMMUTABLE' USING ERRCODE='42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sdk_call_immutable BEFORE UPDATE ON assistant.assistant_sdk_tool_calls
    FOR EACH ROW EXECUTE FUNCTION assistant.guard_sdk_call();

-- Run then SDK-owner lock order matches Runtime/Worker preparation and dispatch.
CREATE FUNCTION assistant.assert_sdk_execution(p_run uuid,p_tenant varchar,p_user varchar,p_session varchar,p_owner uuid,p_fence bigint,p_admission boolean)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,gateway AS $$
DECLARE r assistant.assistant_runs%ROWTYPE; s assistant.assistant_sdk_runs%ROWTYPE;
BEGIN
    SELECT * INTO r FROM assistant_runs WHERE run_id=p_run AND tenant_id=p_tenant AND user_id=p_user
        AND session_id=p_session AND engine IN ('openai_agents','claude_agent_sdk') FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_EXECUTION_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
    SELECT * INTO s FROM assistant_sdk_runs WHERE run_id=r.run_id AND engine=r.engine AND tenant_id=r.tenant_id
        AND user_id=r.user_id AND session_id=r.session_id AND owner_id=p_owner AND fence=p_fence
        AND lease_until>clock_timestamp() FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_EXECUTION_FENCE_LOST' USING ERRCODE='42501'; END IF;
    IF p_admission AND (r.status<>'running' OR s.revoked_at IS NOT NULL OR s.expires_at<=clock_timestamp()
        OR NOT EXISTS(SELECT 1 FROM assistant.sessions se JOIN gateway.sdk_session_barriers b
            ON b.tenant_id=se.tenant_id AND b.user_id=se.user_id AND b.session_id=se.session_id
            JOIN gateway.sdk_session_scopes sc ON sc.tenant_id=se.tenant_id AND sc.user_id=se.user_id AND sc.session_id=se.session_id
            WHERE se.tenant_id=s.tenant_id AND se.user_id=s.user_id AND se.session_id=s.session_id
            AND se.service_id=s.service_id AND se.status<>'deleted' AND (se.expires_at IS NULL OR se.expires_at>clock_timestamp())
            AND NOT b.deleting AND sc.state='active' AND sc.service_id=s.service_id AND sc.framework=s.engine)) THEN
        RAISE EXCEPTION 'SDK_EXECUTION_AUTHORITY_REVOKED' USING ERRCODE='42501';
    END IF;
    IF p_admission THEN
        -- Configuration/status revocation serializes with dispatch itself; the
        -- in-memory provenance check and periodic heartbeat are not sufficient.
        PERFORM 1 FROM gateway.services service WHERE service.service_id=s.service_id AND service.status='active'
            AND service.session_enabled AND service.max_retries=0 AND service.connector_type='http'
            AND service.metadata->>'adapter_type'=s.engine
            AND service.metadata->'sdk_agent'=s.snapshot->'sdk_agent'
            AND COALESCE(service.metadata->'sdk_tools','{}'::jsonb)=COALESCE(s.snapshot->'sdk_tools','{}'::jsonb)
            FOR SHARE;
        IF NOT FOUND THEN RAISE EXCEPTION 'SDK_SERVICE_AUTHORITY_REVOKED' USING ERRCODE='42501'; END IF;
    END IF;
    RETURN TRUE;
END $$;
CREATE FUNCTION assistant.issue_sdk_execution(p_run uuid,p_tenant varchar,p_user varchar,p_session varchar,p_service varchar,p_request varchar,p_engine varchar,p_request_hash char(64),p_snapshot uuid,p_revision bigint,p_payload jsonb,p_hash char(64),p_owner uuid)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant,gateway AS $$
BEGIN
    -- This narrow function is Gateway-only. Public fields never choose the owner or snapshot.
    PERFORM 1 FROM gateway.sdk_session_barriers WHERE tenant_id=p_tenant AND user_id=p_user AND session_id=p_session AND NOT deleting FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_SESSION_UNAVAILABLE' USING ERRCODE='42501'; END IF;
    PERFORM 1 FROM assistant.sessions s WHERE s.tenant_id=p_tenant AND s.user_id=p_user AND s.session_id=p_session AND s.service_id=p_service
        AND s.status<>'deleted' AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp()) FOR SHARE OF s;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_SESSION_UNAVAILABLE' USING ERRCODE='42501'; END IF;
    IF jsonb_typeof(p_payload#>'{readonly_capabilities,capability_allowlist}') IS DISTINCT FROM 'array'
        OR jsonb_array_length(p_payload#>'{readonly_capabilities,capability_allowlist}') NOT BETWEEN 1 AND 16
        OR p_engine NOT IN ('openai_agents','claude_agent_sdk') THEN
        RAISE EXCEPTION 'SDK_SNAPSHOT_INVALID' USING ERRCODE='42501';
    END IF;
    INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id,engine,status) VALUES(p_run,p_tenant,p_user,p_session,p_engine,'running');
    INSERT INTO assistant_sdk_runs(run_id,tenant_id,user_id,session_id,service_id,request_id,engine,request_sha256,snapshot_id,capability_revision,snapshot,snapshot_sha256,owner_id,fence,lease_until,expires_at)
    VALUES(p_run,p_tenant,p_user,p_session,p_service,p_request,p_engine,p_request_hash,p_snapshot,p_revision,p_payload,p_hash,p_owner,1,clock_timestamp()+interval '30 seconds',clock_timestamp()+interval '30 minutes');
    PERFORM assert_sdk_execution(p_run,p_tenant,p_user,p_session,p_owner,1,TRUE);
END $$;
CREATE FUNCTION assistant.renew_sdk_execution(p_run uuid,p_owner uuid,p_fence bigint)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE s assistant.assistant_sdk_runs%ROWTYPE;
BEGIN
    SELECT * INTO s FROM assistant_sdk_runs WHERE run_id=p_run;
    IF NOT FOUND THEN RETURN FALSE; END IF;
    PERFORM assert_sdk_execution(p_run,s.tenant_id,s.user_id,s.session_id,p_owner,p_fence,TRUE);
    UPDATE assistant_sdk_runs SET lease_until=LEAST(expires_at,clock_timestamp()+interval '30 seconds')
        WHERE run_id=p_run AND owner_id=p_owner AND fence=p_fence AND revoked_at IS NULL;
    RETURN FOUND;
END $$;
CREATE FUNCTION assistant.finish_sdk_execution(p_run uuid,p_owner uuid,p_fence bigint,p_status varchar)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF p_status NOT IN ('completed','failed','cancelled') THEN RAISE EXCEPTION 'SDK_STATUS_INVALID'; END IF;
    PERFORM 1 FROM assistant_runs r JOIN assistant_sdk_runs s ON s.run_id=r.run_id
        WHERE r.run_id=p_run AND s.owner_id=p_owner AND s.fence=p_fence FOR UPDATE OF r;
    IF NOT FOUND THEN RETURN FALSE; END IF;
    UPDATE assistant_sdk_runs SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE run_id=p_run;
    UPDATE assistant_runs SET status=p_status,finished_at=clock_timestamp(),updated_at=clock_timestamp()
        WHERE run_id=p_run AND status='running';
    UPDATE assistant_tool_approvals SET status='cancelled',reason='sdk_run_ended',updated_at=clock_timestamp()
        WHERE run_id=p_run AND status IN ('pending','approved');
    RETURN TRUE;
END $$;
CREATE OR REPLACE FUNCTION assistant.assert_worker_execution_owner(p_run UUID,p_tenant VARCHAR,p_user VARCHAR,p_session VARCHAR,p_owner UUID,p_fence BIGINT,p_admission BOOLEAN)
RETURNS BOOLEAN LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    PERFORM 1 FROM assistant_runs WHERE run_id=p_run AND tenant_id=p_tenant
        AND user_id=p_user AND session_id=p_session FOR SHARE;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_WORKER_SCOPE_MISMATCH' USING ERRCODE='42501'; END IF;
    IF EXISTS (SELECT 1 FROM assistant_runs WHERE run_id=p_run AND engine IN ('openai_agents','claude_agent_sdk')) THEN
        IF p_admission AND (EXISTS(SELECT 1 FROM assistant_sdk_tool_calls WHERE run_id=p_run AND response->>'status'='side_effect_unknown')
            OR EXISTS(SELECT 1 FROM assistant_capability_executions WHERE run_id=p_run AND status='side_effect_unknown')) THEN
            RAISE EXCEPTION 'SDK_RUN_RECONCILIATION_REQUIRED' USING ERRCODE='42501';
        END IF;
        RETURN assert_sdk_execution(p_run,p_tenant,p_user,p_session,p_owner,p_fence,p_admission);
    END IF;
    IF EXISTS (SELECT 1 FROM assistant_runtime_execution_owners WHERE run_id=p_run) THEN
        IF p_admission THEN PERFORM assert_runtime_execution(p_run,p_owner,p_fence);
        ELSE PERFORM assert_runtime_execution_fence(p_run,p_owner,p_fence); END IF;
    END IF;
    RETURN TRUE;
END $$;

CREATE OR REPLACE FUNCTION assistant.guard_capability_dispatch_authority() RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, assistant AS $$
BEGIN
    IF NEW.dispatch_fence IS NOT NULL AND NEW.dispatch_fence IS DISTINCT FROM OLD.dispatch_fence THEN
        IF EXISTS(SELECT 1 FROM assistant_runs WHERE run_id=NEW.run_id AND engine IN ('openai_agents','claude_agent_sdk')) THEN
            PERFORM assert_sdk_execution(NEW.run_id,NEW.tenant_id,NEW.user_id,NEW.session_id,s.owner_id,s.fence,TRUE)
                FROM assistant_sdk_runs s JOIN assistant_sdk_tool_calls c ON c.run_id=s.run_id
                WHERE s.run_id=NEW.run_id AND s.capability_revision=NEW.capability_revision
                AND s.snapshot_id::text=NEW.resource_binding->>'snapshot_id'
                AND c.tool_call_id=NEW.tool_call_id AND c.arguments=NEW.arguments AND c.arguments_sha256=NEW.arguments_sha256
                AND c.approval_id IS NOT DISTINCT FROM NEW.approval_id AND c.dispatch_started_at IS NOT NULL
                AND EXISTS(SELECT 1 FROM jsonb_array_elements(s.snapshot#>'{readonly_capabilities,capability_allowlist}') d
                    WHERE d->>'id'=NEW.capability_id AND d->>'name'=c.tool_name);
            IF NOT FOUND THEN RAISE EXCEPTION 'SDK_DISPATCH_AUTHORITY_REVOKED' USING ERRCODE='42501'; END IF;
            -- Check after the authority lock, including a result committed while dispatch waited.
            IF EXISTS(SELECT 1 FROM assistant_sdk_tool_calls WHERE run_id=NEW.run_id AND response->>'status'='side_effect_unknown')
                OR EXISTS(SELECT 1 FROM assistant_capability_executions WHERE run_id=NEW.run_id AND status='side_effect_unknown') THEN
                RAISE EXCEPTION 'SDK_RUN_RECONCILIATION_REQUIRED' USING ERRCODE='42501';
            END IF;
            RETURN NEW;
        END IF;
        PERFORM 1 FROM assistant_runs r JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
            JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
            LEFT JOIN assistant_runtime_execution_owners o ON o.run_id=r.run_id
            WHERE r.run_id=NEW.run_id AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id
            AND r.session_id=NEW.session_id AND r.status='running' AND l.status='active'
            AND l.expires_at>clock_timestamp() AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
            AND (o.run_id IS NULL OR o.lease_until>clock_timestamp())
            AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=s.snapshot_id)
            FOR SHARE OF r,l,s;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'RUNTIME_DISPATCH_AUTHORITY_REVOKED' USING ERRCODE='42501';
        END IF;
    END IF;
    RETURN NEW;
END $$;
-- Cancel SDK authority atomically with session deletion, before worker cleanup.
CREATE FUNCTION assistant.revoke_sdk_session() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE s assistant.assistant_sdk_runs%ROWTYPE;
BEGIN
    IF NEW.deleting AND NOT OLD.deleting THEN
        FOR s IN SELECT * FROM assistant_sdk_runs WHERE tenant_id=NEW.tenant_id AND user_id=NEW.user_id
            AND session_id=NEW.session_id AND revoked_at IS NULL ORDER BY run_id LOOP
            PERFORM finish_sdk_execution(s.run_id,s.owner_id,s.fence,'cancelled');
        END LOOP;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sdk_session_authority_revocation BEFORE UPDATE ON gateway.sdk_session_barriers
    FOR EACH ROW EXECUTE FUNCTION assistant.revoke_sdk_session();
CREATE FUNCTION assistant.decide_sdk_approval(p_id uuid,p_tenant varchar,p_user varchar,p_approved boolean,p_reason text,p_arguments_hash text)
RETURNS text LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE s assistant.assistant_sdk_runs%ROWTYPE; a assistant.assistant_tool_approvals%ROWTYPE; current_status text;
BEGIN
    SELECT s0.* INTO s FROM assistant_sdk_runs s0 JOIN assistant_tool_approvals a0 ON a0.run_id=s0.run_id
        WHERE a0.approval_id=p_id AND a0.tenant_id=p_tenant AND a0.user_id=p_user
        AND s0.tenant_id=p_tenant AND s0.user_id=p_user AND s0.session_id=a0.session_id;
    IF NOT FOUND THEN RAISE EXCEPTION 'SDK_APPROVAL_NOT_FOUND' USING ERRCODE='42501'; END IF;
    PERFORM assert_sdk_execution(s.run_id,s.tenant_id,s.user_id,s.session_id,s.owner_id,s.fence,TRUE);
    SELECT * INTO a FROM assistant_tool_approvals WHERE approval_id=p_id FOR UPDATE;
    IF a.status<>'pending' OR a.expires_at<=clock_timestamp() OR NOT EXISTS (
        SELECT 1 FROM assistant_sdk_tool_calls c WHERE c.run_id=s.run_id AND c.tool_call_id=a.tool_call_id
        AND c.approval_id=a.approval_id AND c.arguments=a.arguments AND c.arguments_sha256=p_arguments_hash
        AND c.dispatch_started_at IS NULL) THEN
        RAISE EXCEPTION 'SDK_APPROVAL_NOT_PENDING' USING ERRCODE='42501';
    END IF;
    current_status:=CASE WHEN p_approved THEN 'approved' ELSE 'rejected' END;
    UPDATE assistant_tool_approvals SET status=current_status,approved_by=p_user,approved_at=clock_timestamp(),
        reason=left(p_reason,1024),updated_at=clock_timestamp() WHERE approval_id=p_id;
    RETURN current_status;
END $$;
REVOKE ALL ON FUNCTION assistant.decide_sdk_approval(uuid,varchar,varchar,boolean,text,text) FROM PUBLIC;
REVOKE ALL ON assistant.assistant_sdk_runs,assistant.assistant_sdk_tool_calls FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.guard_sdk_run(),assistant.guard_sdk_call(),assistant.revoke_sdk_session() FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.assert_sdk_execution(uuid,varchar,varchar,varchar,uuid,bigint,boolean),assistant.issue_sdk_execution(uuid,varchar,varchar,varchar,varchar,varchar,varchar,char,uuid,bigint,jsonb,char,uuid),assistant.renew_sdk_execution(uuid,uuid,bigint),assistant.finish_sdk_execution(uuid,uuid,bigint,varchar) FROM PUBLIC;
DO $permissions$
DECLARE prefix text := left(current_user,length(current_user)-5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN RAISE EXCEPTION 'SDK authority migration requires authority owner'; END IF;
    EXECUTE format('GRANT SELECT ON assistant.assistant_sdk_runs TO %I,%I,%I',prefix||'gateway',prefix||'runtime',prefix||'capability_worker');
    EXECUTE format('GRANT SELECT,INSERT,UPDATE ON assistant.assistant_sdk_tool_calls TO %I',prefix||'runtime');
    EXECUTE format('GRANT SELECT ON assistant.assistant_sdk_tool_calls TO %I',prefix||'gateway');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.decide_sdk_approval(uuid,varchar,varchar,boolean,text,text) TO %I',prefix||'gateway');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.assert_sdk_execution(uuid,varchar,varchar,varchar,uuid,bigint,boolean) TO %I,%I,%I',prefix||'gateway',prefix||'runtime',prefix||'capability_worker');
    EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.issue_sdk_execution(uuid,varchar,varchar,varchar,varchar,varchar,varchar,char,uuid,bigint,jsonb,char,uuid),assistant.renew_sdk_execution(uuid,uuid,bigint),assistant.finish_sdk_execution(uuid,uuid,bigint,varchar) TO %I',prefix||'gateway');
END $permissions$;
