-- T1 item 7 (docs/plans/rag-upgrade-prd-2026-08.md): process-rule snapshots
-- become immutable audit rows. Central-ledger migration (root chain only,
-- PRD §9). Idempotent; additive only.
--
-- Contract implemented here (owned by
-- knowledge_service/persistence/datasets.py record_process_rule /
-- get_process_rule / pin_document_process_rule):
--   * a dataset_process_rules row is a content-addressed snapshot of the
--     chunking dialect that actually built a generation, stored in the
--     canonical dialect: rules = {"chunking": <config>, "processing_mode":
--     <mode>}; the mode column mirrors the dataset's chunking mode
--     (automatic | custom | hierarchical).
--   * rows are written at generation-open (worker _ensure_pipeline_execution
--     for bulk/crash/re-opened rows; the route-side recorder for
--     route-submitted verbs) and referenced from
--     document_pipeline_executions.process_rule_id plus the per-document pin
--     documents.process_rule_id (the replay degrade fallback).
--   * reembed never records a rule: it repairs vectors at existing segment
--     identity and does not run the chunking dialect at all.
--   * because replay verbs (reprocess/recover) and execution rows resolve
--     these rows by id, a row must never change after insert. This migration
--     pins that contract with a BEFORE UPDATE trigger that raises on any
--     update; deletion is deliberately NOT guarded — the 002 FK cascade from
--     datasets must keep working when a dataset is soft/hard removed.
--   * content-dedup by (dataset_id, mode, rules) jsonb equality lives in the
--     writer (best-effort select-then-insert): an unchanged config keeps a
--     stable rule id across generations.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE OR REPLACE FUNCTION guard_dataset_process_rules_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION
        'dataset_process_rules rows are immutable generation snapshots (migration 103); update of % rejected',
        OLD.id;
END;
$$;

-- Alphabetical trigger order fires the guard before the 002 updated_at
-- trigger, so the update dies before any column is rewritten.
DROP TRIGGER IF EXISTS guard_dataset_process_rules_immutable ON dataset_process_rules;
CREATE TRIGGER guard_dataset_process_rules_immutable
    BEFORE UPDATE ON dataset_process_rules
    FOR EACH ROW
    EXECUTE FUNCTION guard_dataset_process_rules_immutable();

COMMENT ON TABLE dataset_process_rules IS
    'Immutable, content-addressed snapshots of the chunking dialect that built a generation '
    '(PRD T1 item 7). Canonical dialect: rules = {"chunking": <config>, "processing_mode": <mode>}. '
    'Rows are written at generation-open, referenced by document_pipeline_executions.process_rule_id '
    'and pinned on documents.process_rule_id; never updated after insert (guard trigger).';
COMMENT ON COLUMN dataset_process_rules.mode IS
    'Chunking mode the generation ran under: automatic | custom | hierarchical';
COMMENT ON COLUMN dataset_process_rules.rules IS
    'Canonical snapshot {"chunking": <chunking config>, "processing_mode": <mode>}; '
    'Dify-shaped payloads are not stored here (documented gap, future rule cascade)';
COMMENT ON COLUMN documents.process_rule_id IS
    'Pinned process-rule snapshot: the replay degrade fallback when an execution row '
    'carries no usable input snapshot (PRD T1 item 7)';
COMMENT ON COLUMN document_pipeline_executions.process_rule_id IS
    'Process-rule snapshot recorded at generation-open for this execution '
    '(NULL for reembed: vector repair runs no chunking dialect) (PRD T1 item 7)';

COMMIT;
