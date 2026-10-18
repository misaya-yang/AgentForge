-- INERT independent authority successor, after expand/write/migrate/retention.
-- Exact legacy bodies below remain selected until this family's explicit expand.
SET LOCAL lock_timeout='2s';SET LOCAL statement_timeout='60s';
CREATE OR REPLACE FUNCTION gateway.cleanup_old_usage_records(retention_days integer DEFAULT 30) RETURNS integer
    LANGUAGE plpgsql
    AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    IF partition_ops.family_enabled('gateway.usage_records') THEN RETURN 0; END IF;
    DELETE FROM usage_records
    WHERE created_at < CURRENT_TIMESTAMP - (retention_days || ' days')::INTERVAL;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$;
CREATE OR REPLACE FUNCTION knowledge.prune_kb_document_progress_events() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'knowledge', 'pg_catalog'
    AS $$
DECLARE
    retained_floor BIGINT;
BEGIN
    IF partition_ops.family_enabled('knowledge.kb_document_progress_events') THEN RETURN NEW; END IF;
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
DO $grants$
DECLARE prefix text; role_name text; candidate text; can_read boolean; can_write boolean; family record;
BEGIN
 PERFORM partition_ops.assert_owner();prefix:=left(current_user,length(current_user)-5);
 REVOKE ALL ON ALL TABLES IN SCHEMA partition_ops FROM PUBLIC;
 REVOKE ALL ON ALL FUNCTIONS IN SCHEMA partition_ops FROM PUBLIC;
 FOREACH candidate IN ARRAY ARRAY['gateway','runtime','capability_worker','knowledge_api','knowledge_worker'] LOOP
  role_name:=prefix||candidate;
  IF NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname=role_name AND NOT rolsuper AND NOT rolbypassrls) THEN RAISE EXCEPTION 'expected bounded native role absent'; END IF;
  can_read:=false;can_write:=false;
  FOR family IN SELECT f.family FROM partition_ops.families f LOOP
   IF to_regclass(family.family) IS NOT NULL THEN
    can_read:=can_read OR has_table_privilege(role_name,family.family,'SELECT') OR has_table_privilege(role_name,family.family,'DELETE');
    can_write:=can_write OR has_table_privilege(role_name,family.family,'INSERT');
   END IF;
  END LOOP;
  IF can_read OR can_write THEN
   EXECUTE format('GRANT USAGE ON SCHEMA partition_ops TO %I',role_name);
   EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.family_enabled(text) TO %I',role_name);
  END IF;
  IF can_write THEN
   EXECUTE format('GRANT EXECUTE ON FUNCTION partition_ops.column_types(text),partition_ops.statement_spec(text,text),partition_ops.require_payload(jsonb),partition_ops.write_json(text,jsonb,text,text,text,jsonb),partition_ops.write_spec(text),partition_ops.lookup_conflict(text,text,jsonb) TO %I',role_name);
  END IF;
 END LOOP;
END $grants$;
