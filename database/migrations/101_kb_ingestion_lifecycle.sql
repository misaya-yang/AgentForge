-- T1 (docs/plans/rag-upgrade-prd-2026-08.md): per-stage ingestion lifecycle
-- state machine + stable chunk identity support + per-document replay
-- snapshots. Central-ledger migration (the per-service twin chain was
-- retired, PRD §9; the root chain is the only deploy path).
--
-- RESTORE-REQUIRED COMPATIBILITY BOUNDARY:
--   This migration replaces UNIQUE(document_id, position) with
--   UNIQUE(document_id, content_type, position). An N-1 writer whose upsert
--   still names ON CONFLICT (document_id, position) is therefore deliberately
--   incompatible after 101 commits. Before applying 101, take and verify a
--   PostgreSQL dump. Recovery to N-1 requires restoring that pre-101 dump into
--   a replacement database before starting the old image. Starting an old
--   image against a post-101 database is not a rollback and must fail closed.
--
-- Contract (PRD T1.1/T1.2 + addendum §1-T1):
--   documents.status : waiting -> parsing -> splitting -> indexing ->
--                      completed | error
--                      (legacy 'syncing' stays for Confluence sync runs)
--   segments.status  : waiting | indexing | completed | error
--                      ('paused' stays reserved in the display vocabulary:
--                       chunk-level pause is a T5 surface and has no writer
--                       yet; segment disable uses enabled=FALSE instead)
--   The API derives display_status (queuing/indexing/paused/error/available/
--   disabled/archived) and never leaks internal states.
--
-- Legacy value mapping (idempotent UPDATEs; safe to re-run):
--   uploaded, queued, processing, detecting -> waiting
--       (full replay is the safe superset under deterministic chunk IDs;
--        detecting rows have persisted nothing yet)
--   segmenting -> splitting
--   embedding, associating_images -> indexing
--   embedding_images -> uploading_images when the row is upload-owned
--       (metadata ? '_document_upload_generation'), else indexing (ingest path)
--   failed -> error
--
-- Reuses existing columns instead of adding duplicates:
--   documents.error            raw error text (already present)
--   documents.process_rule_id  immutable per-document rule pin (002_kbms)
--   segments.enabled           O(1) disable flip (enabled=FALSE +
--                              disabled_by/at); segment status stays
--                              'completed' — no status change is written
--   segments.index_node_id / index_node_hash exist since 002_kbms but never
--   got a writer; the deterministic writer lands in the ingestion code, this
--   migration only adds the lookup index.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

-- 1. documents: per-stage start timestamps. A stage's completion is the next
--    stage's start; completed_at covers the terminal stage. Error text lives
--    in the pre-existing documents.error column.
ALTER TABLE documents ADD COLUMN IF NOT EXISTS parsing_started_at TIMESTAMPTZ;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS splitting_started_at TIMESTAMPTZ;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS indexing_started_at TIMESTAMPTZ;

-- 2. documents: canonical lifecycle vocabulary. Upload-owned rows are split
--    off first: the legacy 'embedding_images' state was written by BOTH the
--    upload path (image embedding before finalize) and the ingest path
--    (multimodal image generation); only the upload-owned ones belong to the
--    upload-phase vocabulary ('uploading_images'), the rest are mid-ingest.
UPDATE documents SET status = 'uploading_images'
WHERE status = 'embedding_images'
  AND COALESCE(metadata, '{}'::jsonb) ? '_document_upload_generation';
UPDATE documents SET status = 'waiting'
WHERE status IN ('uploaded', 'queued', 'processing', 'detecting');
UPDATE documents SET status = 'splitting' WHERE status = 'segmenting';
UPDATE documents SET status = 'indexing'
WHERE status IN ('embedding', 'embedding_images', 'associating_images');
UPDATE documents SET status = 'error'     WHERE status = 'failed';

COMMENT ON COLUMN documents.status IS
  'Lifecycle: waiting|parsing|splitting|indexing|completed|error (+syncing during Confluence runs)';
COMMENT ON COLUMN documents.parsing_started_at IS 'T1 lifecycle: entered parsing';
COMMENT ON COLUMN documents.splitting_started_at IS 'T1 lifecycle: entered splitting';
COMMENT ON COLUMN documents.indexing_started_at IS 'T1 lifecycle: entered indexing';

-- 3. segments: chunk-level raw error text. The status vocabulary
--    (waiting|indexing|completed|error; 'paused' reserved for chunk-level
--    pause operations, no writer yet) is enforced by the ingestion state
--    machine, not a CHECK, so recovery tooling can always write it.
ALTER TABLE segments ADD COLUMN IF NOT EXISTS error TEXT;

-- 4. segments: unified rehash (PRD T1.2). Migration 023 backfilled
--    content_hash with MD5 while the runtime writes sha256(original_text);
--    the two never match, which silently forces full re-embedding and defeats
--    the incremental skip. Recompute sha256 over the stored text (which equals
--    the chunk's original_text in every writer generation) so skip decisions
--    compare like with like. Idempotent: only 32-char legacy digests change.
CREATE EXTENSION IF NOT EXISTS pgcrypto;
UPDATE segments
SET content_hash = encode(digest(text, 'sha256'), 'hex')
WHERE content_hash IS NOT NULL
  AND char_length(content_hash) = 32
  AND text IS NOT NULL;

-- 5. segments: backfill the stable-identity columns for rows written before
--    the deterministic writer existed. index_node_hash mirrors content_hash
--    because the writer sets both from the same chunk digest.
UPDATE segments
SET index_node_id = document_id || '::' || COALESCE(content_type, 'text')
        || '::' || position,
    index_node_hash = content_hash
WHERE index_node_id IS NULL
  AND content_hash IS NOT NULL;

COMMENT ON COLUMN segments.content_hash IS
  'sha256 of chunk original_text (unified by 101; legacy MD5 digests rehashed)';

-- 6. segments: stable-identity lookup for the incremental upsert engine.
CREATE INDEX IF NOT EXISTS idx_segments_index_node_id
    ON segments (index_node_id)
    WHERE index_node_id IS NOT NULL;

-- 7. Cross-table identities used by the replay ledger. The composite keys
--    are redundant with each table's primary key, but make PostgreSQL enforce
--    that a document and a rule belong to the dataset named by an execution.
--    Existing invalid pins fail clearly during VALIDATE instead of becoming a
--    latent cross-dataset replay path.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'documents'::regclass
          AND conname = 'uq_documents_dataset_identity'
    ) THEN
        ALTER TABLE documents
            ADD CONSTRAINT uq_documents_dataset_identity
            UNIQUE (document_id, dataset_id);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'dataset_process_rules'::regclass
          AND conname = 'uq_dataset_process_rules_identity'
    ) THEN
        ALTER TABLE dataset_process_rules
            ADD CONSTRAINT uq_dataset_process_rules_identity
            UNIQUE (id, dataset_id);
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'documents'::regclass
          AND conname = 'fk_documents_process_rule_dataset'
    ) THEN
        ALTER TABLE documents
            ADD CONSTRAINT fk_documents_process_rule_dataset
            FOREIGN KEY (process_rule_id, dataset_id)
            REFERENCES dataset_process_rules (id, dataset_id)
            ON DELETE SET NULL (process_rule_id)
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE documents VALIDATE CONSTRAINT fk_documents_process_rule_dataset;

-- 8. Per-document pipeline execution log = replay snapshot
--    (addendum §1-T1.3: reprocess/recover replay the SNAPSHOT version, never
--    the current published config, so in-flight documents cannot drift).
--    Also carries the staging manifest for revision-atomic flips (PRD T1.5).
CREATE TABLE IF NOT EXISTS document_pipeline_executions (
    execution_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    action VARCHAR(50) NOT NULL,                      -- ingest|reprocess|reembed|recover|retry
    trigger_source VARCHAR(50) NOT NULL DEFAULT 'api',-- upload|api|worker|confluence_sync|recover
    triggered_by VARCHAR(255),
    process_rule_id VARCHAR(255),
    input_snapshot JSONB NOT NULL DEFAULT '{}',
    manifest JSONB NOT NULL DEFAULT '{}',
    status VARCHAR(50) NOT NULL DEFAULT 'running',    -- running|completed|error
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

COMMENT ON TABLE document_pipeline_executions IS
  'T1 per-document pipeline executions: immutable input snapshot for replay + staging manifest';

CREATE INDEX IF NOT EXISTS idx_pipeline_executions_document
    ON document_pipeline_executions (document_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_pipeline_executions_dataset
    ON document_pipeline_executions (dataset_id, created_at DESC);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'document_pipeline_executions'::regclass
          AND conname = 'fk_pipeline_execution_document_dataset'
    ) THEN
        ALTER TABLE document_pipeline_executions
            ADD CONSTRAINT fk_pipeline_execution_document_dataset
            FOREIGN KEY (document_id, dataset_id)
            REFERENCES documents (document_id, dataset_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'document_pipeline_executions'::regclass
          AND conname = 'fk_pipeline_execution_rule_dataset'
    ) THEN
        ALTER TABLE document_pipeline_executions
            ADD CONSTRAINT fk_pipeline_execution_rule_dataset
            FOREIGN KEY (process_rule_id, dataset_id)
            REFERENCES dataset_process_rules (id, dataset_id)
            ON DELETE SET NULL (process_rule_id)
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE document_pipeline_executions
    VALIDATE CONSTRAINT fk_pipeline_execution_document_dataset;
ALTER TABLE document_pipeline_executions
    VALIDATE CONSTRAINT fk_pipeline_execution_rule_dataset;

-- 9. segments: per-content-type position identity (PRD T1.3). The legacy
--    UNIQUE(document_id, position) spans text and image rows in a single
--    namespace. The incremental upsert engine stages new text generations
--    alongside already-persisted image rows (and future parent-child rows),
--    so position identity must be scoped per content_type; otherwise a text
--    upsert can clobber an unrelated row that happens to share its position.
--    The new constraint is strictly looser than the old one, so existing
--    data always satisfies it.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'segments'::regclass
          AND conname = 'segments_document_id_position_key'
    ) THEN
        ALTER TABLE segments DROP CONSTRAINT segments_document_id_position_key;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'segments'::regclass
          AND conname = 'uq_segments_doc_content_position'
    ) THEN
        ALTER TABLE segments
            ADD CONSTRAINT uq_segments_doc_content_position
            UNIQUE (document_id, content_type, position);
    END IF;
END
$$;

COMMIT;
