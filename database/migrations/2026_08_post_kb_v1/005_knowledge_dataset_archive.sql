-- KNO-01: reversible dataset visibility without deleting content or grants.
ALTER TABLE knowledge.datasets
    ADD COLUMN is_archived BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN archived_at TIMESTAMPTZ,
    ADD COLUMN archived_by VARCHAR(255),
    ADD COLUMN archive_reason TEXT;

ALTER TABLE knowledge.datasets
    ADD CONSTRAINT dataset_archive_state_consistent CHECK (
        (is_archived AND archived_at IS NOT NULL AND archived_by IS NOT NULL)
        OR (NOT is_archived AND archived_at IS NULL AND archived_by IS NULL
            AND archive_reason IS NULL)
    );

CREATE INDEX idx_datasets_archived_tenant_created_at
    ON knowledge.datasets (tenant_id, created_at DESC, dataset_id DESC)
    WHERE is_deleted = FALSE AND is_archived = TRUE;
