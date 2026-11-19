-- UNNUMBERED PROPOSAL. Apply only through native migration authority, with an
-- admitted schema epoch and scoped runtime EXECUTE grants. Do not run directly.
-- Existing execution-owner functions/frozen fingerprints are not edited.
CREATE TABLE assistant.assistant_runtime_thread_owners (
    runtime_thread_id uuid PRIMARY KEY REFERENCES assistant.assistant_runtime_threads(runtime_thread_id),
    owner_id uuid NOT NULL,
    address text NOT NULL CHECK (length(address) BETWEEN 1 AND 2048),
    fence bigint NOT NULL CHECK (fence > 0),
    lease_until timestamptz NOT NULL,
    heartbeat_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON assistant.assistant_runtime_thread_owners FROM PUBLIC;

CREATE FUNCTION assistant.claim_runtime_thread_owner(p_thread uuid,p_owner uuid,p_address text)
RETURNS assistant.assistant_runtime_thread_owners
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE claim assistant.assistant_runtime_thread_owners%ROWTYPE;
BEGIN
    IF length(p_address) NOT BETWEEN 1 AND 2048 THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_ADDRESS_INVALID' USING ERRCODE='22023';
    END IF;
    PERFORM 1 FROM assistant_runtime_threads WHERE runtime_thread_id=p_thread AND deleted_at IS NULL FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'RUNTIME_THREAD_NOT_FOUND' USING ERRCODE='55000'; END IF;
    SELECT * INTO claim FROM assistant_runtime_thread_owners WHERE runtime_thread_id=p_thread FOR UPDATE;
    IF FOUND THEN
        -- A valid execution lease remains authoritative even at the boundary
        -- where the thread heartbeat has not yet caught up. Never steal it.
        IF claim.owner_id<>p_owner AND (claim.lease_until>clock_timestamp() OR EXISTS (
            SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs r ON r.run_id=o.run_id
            WHERE o.runtime_thread_id=p_thread AND o.owner_id<>p_owner
              AND o.lease_until>clock_timestamp() AND r.status IN ('running','awaiting_approval')
        )) THEN RETURN claim; END IF;
        IF claim.owner_id=p_owner AND claim.address<>p_address THEN
            RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_ADDRESS_CHANGED' USING ERRCODE='42501';
        END IF;
        UPDATE assistant_runtime_thread_owners SET owner_id=p_owner,address=p_address,
            fence=fence+CASE WHEN owner_id=p_owner THEN 0 ELSE 1 END,
            lease_until=clock_timestamp()+INTERVAL '30 seconds',heartbeat_at=clock_timestamp()
            WHERE runtime_thread_id=p_thread RETURNING * INTO claim;
    ELSE
        -- Mixed old/new runtime ownership cannot invent the old owner's URL.
        IF EXISTS (SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs r ON r.run_id=o.run_id
            WHERE o.runtime_thread_id=p_thread AND o.owner_id<>p_owner AND o.lease_until>clock_timestamp()
              AND r.status IN ('running','awaiting_approval')) THEN
            RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_ADDRESS_UNKNOWN' USING ERRCODE='55000';
        END IF;
        INSERT INTO assistant_runtime_thread_owners(runtime_thread_id,owner_id,address,fence,lease_until)
            VALUES(p_thread,p_owner,p_address,1,clock_timestamp()+INTERVAL '30 seconds') RETURNING * INTO claim;
    END IF;
    RETURN claim;
END $$;

CREATE FUNCTION assistant.assert_runtime_thread_owner_for_run(p_run uuid,p_owner uuid,p_address text)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
DECLARE r assistant.assistant_runs%ROWTYPE;
    claim assistant.assistant_runtime_thread_owners%ROWTYPE;
BEGIN
    -- Preserve the existing global lock order: run -> root thread -> owner.
    SELECT * INTO r FROM assistant_runs WHERE run_id=p_run FOR UPDATE;
    IF NOT FOUND OR r.engine<>'agent_runtime' OR r.status NOT IN ('running','awaiting_approval') THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_RUN_NOT_ACTIVE' USING ERRCODE='55000';
    END IF;
    SELECT * INTO claim FROM claim_runtime_thread_owner(r.harness_thread_id,p_owner,p_address);
    IF claim.owner_id<>p_owner OR claim.address<>p_address OR claim.lease_until<=clock_timestamp() THEN
        RAISE EXCEPTION 'RUNTIME_THREAD_OWNER_LOST' USING ERRCODE='42501';
    END IF;
    RETURN true;
END $$;
CREATE FUNCTION assistant.heartbeat_runtime_thread_owner(p_thread uuid,p_owner uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,assistant AS $$
BEGIN
    UPDATE assistant_runtime_thread_owners SET lease_until=clock_timestamp()+INTERVAL '30 seconds',heartbeat_at=clock_timestamp()
      WHERE runtime_thread_id=p_thread AND owner_id=p_owner AND lease_until>clock_timestamp();
    RETURN FOUND;
END $$;
REVOKE ALL ON FUNCTION assistant.claim_runtime_thread_owner(uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.assert_runtime_thread_owner_for_run(uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION assistant.heartbeat_runtime_thread_owner(uuid,uuid) FROM PUBLIC;
-- Authority integration must assign functions/table to canonical non-login owner;
-- runtime gets only SELECT(table), EXECUTE(the three functions); no direct writes.
-- Add exact ownership/ACL and positive/negative scoped role witnesses before activation.
