-- SOURCE CANDIDATE: planned successor019, not activated or authorized to install.
-- Requires explicit review of this SECURITY DEFINER change and canonical admission.
-- Never apply directly to the preserved fixture, rewrite frozen history, grant
-- application ledger writes, or run outside the canonical owner/migrator path.
-- Follows the separately fingerprinted018 metadata reader; see successor-chain design.
DO $owner_check$
DECLARE
    function_owner oid;
    routine oid := 'knowledge.record_kb_document_progress_event()'::regprocedure;
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user <> (SELECT pg_get_userbyid(nspowner)
                           FROM pg_namespace WHERE nspname='knowledge') THEN
        RAISE EXCEPTION 'canonical Knowledge owner scope required';
    END IF;
    SELECT proowner INTO function_owner FROM pg_proc WHERE oid=routine;
    IF function_owner <> (SELECT oid FROM pg_roles WHERE rolname=current_user)
       OR EXISTS (SELECT 1 FROM pg_roles WHERE oid=function_owner
                  AND (rolcanlogin OR rolsuper OR rolcreaterole OR rolcreatedb
                       OR rolreplication OR rolbypassrls)) THEN
        RAISE EXCEPTION 'canonical unprivileged NOLOGIN function owner required';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_proc WHERE oid=routine
                   AND prorettype='trigger'::regtype AND pronargs=0 AND NOT prosecdef)
       OR EXISTS (
          SELECT 1 FROM pg_proc p,
          LATERAL aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) acl
          WHERE p.oid=routine AND acl.grantee<>p.proowner
       ) THEN
        RAISE EXCEPTION 'unexpected progress routine security preimage';
    END IF;
END
$owner_check$;

CREATE OR REPLACE FUNCTION knowledge.record_kb_document_progress_event() RETURNS trigger
    LANGUAGE plpgsql SECURITY DEFINER
    SET search_path TO pg_catalog, pg_temp
    AS $$
DECLARE
    display_state TEXT;
    current_stage TEXT;
    terminal_state BOOLEAN;
    event_payload JSONB;
BEGIN
    -- This owner-powered routine is only the existing document-row projection.
    -- Binding the relation and trigger shape prevents use as a general event writer.
    IF TG_RELID <> 'knowledge.documents'::regclass
       OR TG_LEVEL <> 'ROW' OR TG_WHEN <> 'AFTER' OR TG_NARGS <> 0
       OR TG_OP NOT IN ('INSERT', 'UPDATE', 'DELETE') THEN
        RAISE EXCEPTION 'KNOWLEDGE_PROGRESS_TRIGGER_SCOPE_INVALID'
            USING ERRCODE = '42501';
    END IF;

    IF TG_OP = 'DELETE' THEN
        INSERT INTO knowledge.kb_document_progress_events (
            dataset_id, document_id, event_type, payload
        )
        VALUES (
            OLD.dataset_id,
            OLD.document_id,
            'deleted',
            jsonb_build_object(
                'document_id', OLD.document_id,
                'progress', jsonb_build_object(
                    'percent', 100,
                    'stage', 'deleted',
                    'state', 'deleted'
                ),
                'terminal', TRUE
            )
        );
        RETURN OLD;
    END IF;

    -- UPDATE triggers also fire for content/metadata writes that do not alter
    -- the progress contract.  Avoid creating replay noise for those writes.
    IF TG_OP = 'UPDATE'
       AND ROW(
            NEW.status,
            NEW.progress,
            NEW.error,
            NEW.metadata,
            NEW.enabled,
            NEW.disabled_at,
            NEW.disabled_by,
            NEW.archived,
            NEW.archived_reason,
            NEW.archived_by,
            NEW.archived_at,
            NEW.started_at,
            NEW.completed_at,
            NEW.parsing_started_at,
            NEW.splitting_started_at,
            NEW.indexing_started_at
       ) IS NOT DISTINCT FROM ROW(
            OLD.status,
            OLD.progress,
            OLD.error,
            OLD.metadata,
            OLD.enabled,
            OLD.disabled_at,
            OLD.disabled_by,
            OLD.archived,
            OLD.archived_reason,
            OLD.archived_by,
            OLD.archived_at,
            OLD.started_at,
            OLD.completed_at,
            OLD.parsing_started_at,
            OLD.splitting_started_at,
            OLD.indexing_started_at
       ) THEN
        RETURN NEW;
    END IF;

    display_state := CASE
        WHEN COALESCE(NEW.archived, FALSE) THEN 'archived'
        WHEN NEW.status IN ('error', 'failed') THEN 'error'
        WHEN NEW.status = 'paused' THEN 'paused'
        WHEN NEW.status = 'completed' AND COALESCE(NEW.enabled, TRUE) = FALSE
            THEN 'disabled'
        WHEN NEW.status = 'completed' THEN 'available'
        WHEN NEW.status IN ('waiting', 'queued', 'pending', 'uploaded', 'detecting')
            THEN 'queuing'
        ELSE 'indexing'
    END;
    current_stage := CASE
        WHEN NEW.status IN ('waiting', 'queued', 'pending', 'uploaded', 'detecting')
            THEN 'waiting'
        WHEN NEW.status IN ('parsing', 'splitting', 'indexing') THEN NEW.status
        WHEN NEW.status IN ('completed', 'error', 'failed') THEN NEW.status
        ELSE 'indexing'
    END;
    terminal_state := NEW.status IN ('completed', 'error', 'failed');
    event_payload := jsonb_build_object(
        'document_id', NEW.document_id,
        'progress', jsonb_build_object(
            'percent', GREATEST(0::NUMERIC, LEAST(100::NUMERIC, COALESCE(NEW.progress, 0))),
            'stage', current_stage,
            'state', display_state
        ),
        'terminal', terminal_state,
        'error', CASE WHEN NEW.status IN ('error', 'failed')
            THEN LEFT(COALESCE(NEW.error, ''), 2000) ELSE NULL END
    );

    INSERT INTO knowledge.kb_document_progress_events (
        dataset_id, document_id, event_type, payload
    )
    VALUES (
        NEW.dataset_id,
        NEW.document_id,
        CASE WHEN terminal_state THEN 'terminal' ELSE 'progress' END,
        event_payload
    );
    RETURN NEW;
END;
$$;

-- CREATE OR REPLACE preserves the already-verified owner-only ACL. No application
-- EXECUTE, event-ledger DML, sequence, schema, membership or default grants arise.
REVOKE ALL ON FUNCTION knowledge.record_kb_document_progress_event() FROM PUBLIC;
