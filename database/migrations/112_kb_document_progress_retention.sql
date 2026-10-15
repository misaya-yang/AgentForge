-- T5/H1 #4 follow-up: bound durable document-progress replay storage.
--
-- Fresh SSE connections start at the current dataset watermark in application
-- code.  This database policy retains enough history for normal reconnects
-- while preventing the append-only ledger from growing without limit.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE INDEX IF NOT EXISTS idx_kb_document_progress_events_dataset_created
    ON knowledge.kb_document_progress_events (dataset_id, created_at);

CREATE OR REPLACE FUNCTION knowledge.prune_kb_document_progress_events()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = knowledge, pg_catalog
AS $$
DECLARE
    retained_floor BIGINT;
BEGIN
    -- Seven days is longer than the browser reconnect contract. Run the indexed
    -- age prune on each new transition so an inactive dataset needs no scheduler.
    DELETE FROM knowledge.kb_document_progress_events
    WHERE dataset_id = NEW.dataset_id
      AND created_at < NOW() - INTERVAL '7 days';

    -- The age window can still be noisy during bulk ingestion. Amortize the
    -- per-dataset row cap to one indexed check per 128 global events.
    IF MOD(NEW.event_sequence, 128) = 0 THEN
        SELECT MIN(recent.event_sequence)
        INTO retained_floor
        FROM (
            SELECT event_sequence
            FROM knowledge.kb_document_progress_events
            WHERE dataset_id = NEW.dataset_id
            ORDER BY event_sequence DESC
            LIMIT 10000
        ) AS recent;

        IF retained_floor IS NOT NULL THEN
            DELETE FROM knowledge.kb_document_progress_events
            WHERE dataset_id = NEW.dataset_id
              AND event_sequence < retained_floor;
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_trigger
        WHERE tgname = 'trg_kb_document_progress_retention'
          AND tgrelid = 'knowledge.kb_document_progress_events'::regclass
    ) THEN
        CREATE TRIGGER trg_kb_document_progress_retention
            AFTER INSERT ON knowledge.kb_document_progress_events
            FOR EACH ROW
            EXECUTE FUNCTION knowledge.prune_kb_document_progress_events();
    END IF;
END
$$;

COMMENT ON FUNCTION knowledge.prune_kb_document_progress_events() IS
  'Retains seven days and at most roughly 10k replay events per dataset';

COMMIT;
