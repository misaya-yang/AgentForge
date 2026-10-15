-- Durable, fair document batch operations (PRD H1 #7 / T5-8).
--
-- The operation row is the tenant-scoped receipt exposed to the UI. Items are
-- intentionally not foreign-keyed to documents: a successful delete must
-- retain its audit/progress receipt after the document row is gone. Workers
-- claim one item at a time with FOR UPDATE SKIP LOCKED and rotate operations
-- through last_claimed_at, so a large batch cannot monopolize the queue.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE TABLE IF NOT EXISTS kb_document_batch_operations (
    operation_id   UUID PRIMARY KEY,
    tenant_id      VARCHAR(255) NOT NULL,
    dataset_id     VARCHAR(255) NOT NULL,
    operation      VARCHAR(32) NOT NULL
        CHECK (operation IN ('reembed', 'delete')),
    status         VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'partial', 'failed')),
    total_count    INTEGER NOT NULL DEFAULT 0 CHECK (total_count >= 0),
    queued_count   INTEGER NOT NULL DEFAULT 0 CHECK (queued_count >= 0),
    skipped_count  INTEGER NOT NULL DEFAULT 0 CHECK (skipped_count >= 0),
    failed_count   INTEGER NOT NULL DEFAULT 0 CHECK (failed_count >= 0),
    created_by     VARCHAR(255) NOT NULL,
    actor_roles    JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(actor_roles) = 'array'),
    last_claimed_at TIMESTAMPTZ,
    error          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at   TIMESTAMPTZ,
    CONSTRAINT fk_kb_document_batch_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets(dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT kb_document_batch_count_bounds CHECK (
        queued_count + skipped_count + failed_count <= total_count
    )
);

-- Existing pre-release tables may have only the dataset_id FK. Add and
-- validate the complete tenant identity without trusting the API layer.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'kb_document_batch_operations'::regclass
          AND conname = 'fk_kb_document_batch_dataset_tenant'
    ) THEN
        ALTER TABLE kb_document_batch_operations
            ADD CONSTRAINT fk_kb_document_batch_dataset_tenant
            FOREIGN KEY (dataset_id, tenant_id)
            REFERENCES datasets(dataset_id, tenant_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE kb_document_batch_operations
    VALIDATE CONSTRAINT fk_kb_document_batch_dataset_tenant;
ALTER TABLE kb_document_batch_operations
    DROP CONSTRAINT IF EXISTS fk_kb_document_batch_dataset;

CREATE TABLE IF NOT EXISTS kb_document_batch_items (
    operation_id UUID NOT NULL,
    document_id  VARCHAR(255) NOT NULL,
    ordinal      INTEGER NOT NULL CHECK (ordinal >= 0),
    status       VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'claiming', 'queued', 'skipped', 'failed')),
    claimed_by   VARCHAR(255),
    claimed_at   TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error_code   VARCHAR(64),
    error        TEXT,
    PRIMARY KEY (operation_id, document_id),
    UNIQUE (operation_id, ordinal),
    CONSTRAINT fk_kb_document_batch_item_operation
        FOREIGN KEY (operation_id)
        REFERENCES kb_document_batch_operations(operation_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_kb_document_batch_fair_claim
    ON kb_document_batch_operations (last_claimed_at NULLS FIRST, created_at)
    WHERE status IN ('pending', 'running');

CREATE INDEX IF NOT EXISTS idx_kb_document_batch_scope
    ON kb_document_batch_operations (tenant_id, dataset_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_kb_document_batch_item_claim
    ON kb_document_batch_items (operation_id, status, ordinal);

COMMENT ON TABLE kb_document_batch_operations IS
    'PRD H1 #7: tenant-scoped durable reembed/delete batch receipt, fairly claimed by workers';
COMMENT ON TABLE kb_document_batch_items IS
    'PRD H1 #7: per-document durable batch outcome retained after document deletion';

COMMIT;
