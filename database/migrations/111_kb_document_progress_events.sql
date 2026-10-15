-- T5/H1 #4: durable document-progress events for SSE replay.
--
-- The document row remains the source of truth.  This append-only, dataset-
-- scoped ledger records the progress transitions that the row exposes so a
-- client reconnecting with Last-Event-ID can replay events after a process or
-- network interruption.  The trigger covers every document writer, including
-- workers and lifecycle operations in other processes.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE TABLE IF NOT EXISTS kb_document_progress_events (
    event_sequence BIGSERIAL PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL,
    document_id VARCHAR(255) NOT NULL,
    event_type VARCHAR(20) NOT NULL
        CHECK (event_type IN ('progress', 'terminal', 'deleted')),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE kb_document_progress_events IS
  'T5/H1 #4 append-only document progress ledger; event IDs are dataset_id:event_sequence';

CREATE INDEX IF NOT EXISTS idx_kb_document_progress_events_dataset_sequence
    ON kb_document_progress_events (dataset_id, event_sequence);
CREATE INDEX IF NOT EXISTS idx_kb_document_progress_events_document_sequence
    ON kb_document_progress_events (dataset_id, document_id, event_sequence);

CREATE OR REPLACE FUNCTION knowledge.record_kb_document_progress_event()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = knowledge, pg_catalog
AS $$
DECLARE
    display_state TEXT;
    current_stage TEXT;
    terminal_state BOOLEAN;
    event_payload JSONB;
BEGIN
    IF TG_OP = 'DELETE' THEN
        INSERT INTO kb_document_progress_events (
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

    INSERT INTO kb_document_progress_events (
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

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_trigger
        WHERE tgname = 'trg_kb_document_progress_events'
          AND tgrelid = 'documents'::regclass
    ) THEN
        CREATE TRIGGER trg_kb_document_progress_events
            AFTER INSERT OR UPDATE OR DELETE ON documents
            FOR EACH ROW
            EXECUTE FUNCTION knowledge.record_kb_document_progress_event();
    END IF;
END
$$;

COMMIT;
