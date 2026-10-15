-- ============================================================
-- 032: Dataset soft delete and deletion audit metadata
-- ============================================================

ALTER TABLE datasets
    ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deleted_by VARCHAR(255),
    ADD COLUMN IF NOT EXISTS delete_reason TEXT;

UPDATE datasets
SET is_deleted = FALSE
WHERE is_deleted IS NULL;

CREATE INDEX IF NOT EXISTS idx_datasets_active_created_at
    ON datasets(created_at DESC)
    WHERE is_deleted = FALSE;

CREATE INDEX IF NOT EXISTS idx_datasets_active_tenant_visibility
    ON datasets(tenant_id, visibility, created_at DESC)
    WHERE is_deleted = FALSE;
