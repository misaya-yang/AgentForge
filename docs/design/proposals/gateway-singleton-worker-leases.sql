-- U-GW-12 native-authority proposal. Install as a NEW ordered migration;
-- never source this from application startup or edit frozen migrations.
-- Run as ai_gateway_owner through the native authority; prefix-render ACL roles.
CREATE TABLE gateway.singleton_worker_leases (
    job text PRIMARY KEY CHECK (job IN (
        'eval_outbox_worker','usage_scheduler','scheduled_task_dispatcher',
        'image_task_worker','file_cleanup_service','sdk_session_cleanup',
        'trace_retention_scheduler','assistant_trace_reconciler_task','usage_event_consumer')),
    owner uuid NOT NULL,
    fence bigint NOT NULL CHECK (fence > 0),
    expires_at timestamptz NOT NULL
);
REVOKE ALL ON gateway.singleton_worker_leases FROM PUBLIC;
-- No application DML: every transition is a bounded SECURITY DEFINER function.
CREATE FUNCTION gateway.acquire_worker_lease(p_job text, p_owner uuid, p_ttl_ms bigint)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE result bigint; inserted bigint;
BEGIN
    IF p_ttl_ms < 1000 OR p_ttl_ms > 300000 OR p_owner IS NULL THEN
        RAISE EXCEPTION 'invalid worker lease';
    END IF;
    INSERT INTO gateway.singleton_worker_leases(job,owner,fence,expires_at)
        VALUES(p_job,p_owner,1,clock_timestamp()+p_ttl_ms*interval '1 millisecond')
        ON CONFLICT (job) DO NOTHING;
    GET DIAGNOSTICS inserted = ROW_COUNT;
    IF inserted=1 THEN RETURN 1; END IF;
    -- Explicit FOR UPDATE conflicts with effect KEY SHARE locks. NOWAIT
    -- keeps a contender from queuing ahead of the current owner's renewal.
    BEGIN
        SELECT fence INTO result FROM gateway.singleton_worker_leases
            WHERE job=p_job FOR UPDATE NOWAIT;
    EXCEPTION WHEN lock_not_available THEN RETURN NULL;
    END;
    UPDATE gateway.singleton_worker_leases
        SET owner=p_owner,fence=fence+1,
            expires_at=clock_timestamp()+p_ttl_ms*interval '1 millisecond'
        WHERE job=p_job AND expires_at<=clock_timestamp()
        RETURNING fence INTO result;
    RETURN result;
END $$;
CREATE FUNCTION gateway.renew_worker_lease(p_job text,p_owner uuid,p_fence bigint,p_ttl_ms bigint)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF p_ttl_ms < 1000 OR p_ttl_ms > 300000 THEN
        RAISE EXCEPTION 'invalid worker lease';
    END IF;
    -- Updating non-key expiry takes NO KEY UPDATE, compatible with effect
    -- KEY SHARE. Expired leases can never be resurrected, even by their owner.
    UPDATE gateway.singleton_worker_leases
        SET expires_at=clock_timestamp()+p_ttl_ms*interval '1 millisecond'
        WHERE job=p_job AND owner=p_owner AND fence=p_fence
          AND expires_at>clock_timestamp();
    RETURN FOUND;
END $$;
CREATE FUNCTION gateway.assert_worker_lease(p_job text,p_owner uuid,p_fence bigint)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    PERFORM 1 FROM gateway.singleton_worker_leases
        WHERE job=p_job AND owner=p_owner AND fence=p_fence
          AND expires_at>clock_timestamp() FOR KEY SHARE;
    RETURN FOUND;
END $$;
CREATE FUNCTION gateway.release_worker_lease(p_job text,p_owner uuid,p_fence bigint)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    -- Release runs only after job stop AND effect drain. Serialize it with
    -- effects too; never remove the row (fences must remain monotonic).
    PERFORM 1 FROM gateway.singleton_worker_leases
        WHERE job=p_job AND owner=p_owner AND fence=p_fence FOR UPDATE;
    UPDATE gateway.singleton_worker_leases SET expires_at=clock_timestamp()
        WHERE job=p_job AND owner=p_owner AND fence=p_fence;
    RETURN FOUND;
END $$;
REVOKE ALL ON FUNCTION gateway.acquire_worker_lease(text,uuid,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.renew_worker_lease(text,uuid,bigint,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.assert_worker_lease(text,uuid,bigint) FROM PUBLIC;
REVOKE ALL ON FUNCTION gateway.release_worker_lease(text,uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION gateway.acquire_worker_lease(text,uuid,bigint),
    gateway.renew_worker_lease(text,uuid,bigint,bigint),
    gateway.assert_worker_lease(text,uuid,bigint),
    gateway.release_worker_lease(text,uuid,bigint) TO ai_gateway_gateway;
