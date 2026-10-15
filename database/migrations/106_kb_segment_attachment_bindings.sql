-- T1 addendum (docs/plans/rag-upgrade-prd-addendum-dify-2026-08.md
-- §1-T1.7): tenant-scoped segment-to-attachment ownership.
--
-- The central migration ledger is the only deploy path (PRD §9).  Bindings
-- are derived receipts, but they are still protected by the complete
-- dataset/document/segment ownership chain.  The runtime replacement hook
-- updates them only inside a successful document publication transaction;
-- ON DELETE CASCADE is the delayed-cleanup backstop for deleted segments.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'segments'::regclass
          AND conname = 'uq_segments_document_dataset_identity'
    ) THEN
        ALTER TABLE segments
            ADD CONSTRAINT uq_segments_document_dataset_identity
            UNIQUE (segment_id, document_id, dataset_id);
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS knowledge.kb_segment_attachment_bindings (
    tenant_id      VARCHAR(255) NOT NULL,
    dataset_id     VARCHAR(255) NOT NULL,
    document_id    VARCHAR(255) NOT NULL,
    segment_id     VARCHAR(255) NOT NULL,
    attachment_id  VARCHAR(255) NOT NULL,
    capabilities   JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT pk_kb_segment_attachment_bindings PRIMARY KEY
        (tenant_id, dataset_id, document_id, segment_id, attachment_id),
    CONSTRAINT chk_kb_segment_attachment_capabilities_array
        CHECK (jsonb_typeof(capabilities) = 'array'),
    CONSTRAINT fk_kb_segment_attachment_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT fk_kb_segment_attachment_document_dataset
        FOREIGN KEY (document_id, dataset_id)
        REFERENCES documents (document_id, dataset_id) ON DELETE CASCADE,
    CONSTRAINT fk_kb_segment_attachment_segment_document_dataset
        FOREIGN KEY (segment_id, document_id, dataset_id)
        REFERENCES segments (segment_id, document_id, dataset_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_kb_segment_attachment_owner
    ON knowledge.kb_segment_attachment_bindings
        (tenant_id, dataset_id, document_id, segment_id);
CREATE INDEX IF NOT EXISTS idx_kb_segment_attachment_attachment
    ON knowledge.kb_segment_attachment_bindings (attachment_id);

COMMENT ON TABLE knowledge.kb_segment_attachment_bindings IS
  'T1 multimodal receipt: tenant/dataset/document/segment attachment ownership; replaced atomically after successful document publication';
COMMENT ON COLUMN knowledge.kb_segment_attachment_bindings.capabilities IS
  'Attachment capability flags (for example vision); an array by contract';

COMMIT;
