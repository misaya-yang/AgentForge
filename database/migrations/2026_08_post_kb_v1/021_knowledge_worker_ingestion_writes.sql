-- Isolated draft only. Do not apply directly or copy into canonical migrations.
-- Candidate canonical successor 021: permit the Knowledge worker to open and
-- close only the writes required by its normal ingestion generation chain.
--
-- Source evidence:
--   migration_knowledge_ingest/service.rs::prepare_execution calls
--   KnowledgeWriteStore::record_process_rule, which SELECTs and INSERTs
--   knowledge.dataset_process_rules before document parsing starts. Migration
--   103 makes these generation snapshots immutable, so the worker needs INSERT
--   but must not receive UPDATE or DELETE.
--   migration_knowledge_ingest/store.rs::complete_execution writes
--   COMPLETE_PIPELINE_EXECUTION_1 after processing; the SQL updates
--   knowledge.document_pipeline_executions.status/error/manifest/completed_at.
--   The worker already has SELECT and INSERT on that table, so it needs UPDATE
--   to terminalize its own ingestion execution. No DELETE is required.
--
-- Read-only confirmation used the native worker's own private DSN:
--   knowledge.dataset_process_rules: SELECT=true, INSERT=false
--   knowledge.document_pipeline_executions: SELECT=true, INSERT=true, UPDATE=false
-- The 017 frozen grants remain unchanged; this draft is a forward-only delta.
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Knowledge worker generation grants require the authority owner role';
    END IF;
    EXECUTE format(
        'GRANT INSERT ON TABLE knowledge.dataset_process_rules TO %I',
        prefix || 'knowledge_worker'
    );
    EXECUTE format(
        'GRANT UPDATE ON TABLE knowledge.document_pipeline_executions TO %I',
        prefix || 'knowledge_worker'
    );
END
$permissions$;
