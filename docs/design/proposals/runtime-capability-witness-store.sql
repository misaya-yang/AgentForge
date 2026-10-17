-- INACTIVE PROPOSAL ONLY. Native authority must assign a successor and grants,
-- verify frozen epoch compatibility, update admission and activate separately.
-- Never source this file directly into an existing developer/production DB.
-- Required principle: Worker/Knowledge/public roles cannot read attenuation seeds.
CREATE TABLE assistant.assistant_runtime_capability_witnesses (
    run_id UUID PRIMARY KEY REFERENCES assistant.assistant_runs(run_id) ON DELETE CASCADE,
    tenant_id VARCHAR NOT NULL,
    user_id VARCHAR NOT NULL,
    session_id VARCHAR NOT NULL,
    snapshot_id UUID NOT NULL,
    managed_snapshot_id UUID GENERATED ALWAYS AS (CASE WHEN token#>>'{root,purpose}'='capability-ceiling' THEN snapshot_id END) STORED REFERENCES assistant.assistant_runtime_snapshots(snapshot_id) ON DELETE CASCADE,
    sdk_snapshot_id UUID GENERATED ALWAYS AS (CASE WHEN token#>>'{root,purpose}'='sdk-capability-ceiling' THEN snapshot_id END) STORED REFERENCES assistant.assistant_sdk_runs(snapshot_id) ON DELETE CASCADE,
    token JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (jsonb_typeof(token)='object'),
    CHECK ((token#>>'{root,purpose}' IN ('capability-ceiling','sdk-capability-ceiling')) IS TRUE),
    CHECK (octet_length(token::text) <= 262144),
    CHECK ((token->>'schema_version'='capability-attenuation/v3') IS TRUE),
    CHECK ((token#>>'{proof,kind}'='open') IS TRUE),
    CHECK ((token#>>'{root,claims,run_id}'=run_id::text) IS TRUE),
    CHECK ((token#>>'{root,claims,snapshot_id}'=snapshot_id::text) IS TRUE),
    CHECK ((token#>>'{root,claims,tenant_id}'=tenant_id) IS TRUE),
    CHECK ((token#>>'{root,claims,user_id}'=user_id) IS TRUE),
    CHECK ((token#>>'{root,claims,session_id}'=session_id) IS TRUE)
);
REVOKE ALL ON assistant.assistant_runtime_capability_witnesses FROM PUBLIC;
-- Deployment-specific role names MUST be resolved by native authority, never
-- pasted as grants to PUBLIC or inherited groups. Requirements, not active SQL:
-- Gateway: INSERT, SELECT; Runtime: SELECT; capability_worker/Knowledge: NONE.
-- Retention/deletion is a separate scoped authority operation, never a consumer
-- UPDATE/DELETE privilege. Add actual-role positive/negative/frozen verification.

-- Cross-table scope, immutable public envelope and actual engine must agree.
-- Both branches retain real snapshot FKs; SDK uses assistant_sdk_runs, which
-- issue_sdk_execution already links to the shared assistant_runs authority.
CREATE FUNCTION assistant.assert_capability_witness_binding() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    IF NEW.token#>>'{root,purpose}'='capability-ceiling' THEN
        IF NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshots s JOIN assistant_runs r ON r.run_id=s.run_id
            WHERE s.snapshot_id=NEW.snapshot_id AND s.run_id=NEW.run_id AND r.runtime_snapshot_id=s.snapshot_id
            AND r.engine='agent_runtime' AND s.tenant_id=NEW.tenant_id AND s.user_id=NEW.user_id AND s.session_id=NEW.session_id
            AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id AND r.session_id=NEW.session_id
            AND NEW.token->'root'=s.snapshot->'capability_ceiling'
            AND NEW.token#>>'{root,claims,runtime_thread_id}'=s.runtime_thread_id::text
            AND NEW.token#>>'{root,claims,capability_revision}'=s.capability_revision::text) THEN
            RAISE EXCEPTION 'CAPABILITY_WITNESS_BINDING_INVALID' USING ERRCODE='42501';
        END IF;
    ELSE
        IF NOT EXISTS (SELECT 1 FROM assistant_sdk_runs s JOIN assistant_runs r ON r.run_id=s.run_id
            WHERE s.snapshot_id=NEW.snapshot_id AND s.run_id=NEW.run_id AND r.engine=s.engine
            AND s.engine IN ('openai_agents','claude_agent_sdk')
            AND s.tenant_id=NEW.tenant_id AND s.user_id=NEW.user_id AND s.session_id=NEW.session_id
            AND r.tenant_id=NEW.tenant_id AND r.user_id=NEW.user_id AND r.session_id=NEW.session_id
            AND s.owner_id IS NOT NULL AND s.fence>0
            AND NEW.token->'root'=s.snapshot->'capability_ceiling'
            AND NEW.token#>>'{root,claims,runtime_thread_id}'=''
            AND NEW.token#>>'{root,claims,capability_revision}'=s.capability_revision::text) THEN
            RAISE EXCEPTION 'CAPABILITY_WITNESS_BINDING_INVALID' USING ERRCODE='42501';
        END IF;
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION assistant.assert_capability_witness_binding() FROM PUBLIC;
CREATE TRIGGER capability_witness_binding BEFORE INSERT OR UPDATE ON assistant.assistant_runtime_capability_witnesses
FOR EACH ROW EXECUTE FUNCTION assistant.assert_capability_witness_binding();
