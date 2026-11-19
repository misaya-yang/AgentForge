-- ============================================================
-- Migration: 025_document_version_control
-- Description: Add version control for documents (especially Confluence sync)
-- Version: 2.3.0
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. Document Versions Table
-- ============================================================
CREATE TABLE IF NOT EXISTS document_versions (
    version_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,

    -- Version info
    version_number INTEGER NOT NULL,
    content TEXT NOT NULL,
    content_hash VARCHAR(64) NOT NULL,

    -- Source info (Confluence)
    confluence_version INTEGER,
    confluence_updated_at TIMESTAMPTZ,

    -- Metadata snapshot
    title VARCHAR(512),
    metadata JSONB DEFAULT '{}'::jsonb,
    word_count INTEGER DEFAULT 0,

    -- Change info
    change_type VARCHAR(50) NOT NULL,  -- created | updated | restored | deleted
    change_reason TEXT,
    changed_by VARCHAR(255),

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Constraints
    UNIQUE(document_id, version_number)
);

COMMENT ON TABLE document_versions IS '文档版本历史表：存储文档内容的历史快照';
COMMENT ON COLUMN document_versions.version_number IS '版本号，每个文档独立递增';
COMMENT ON COLUMN document_versions.content_hash IS '内容 SHA256 哈希，用于去重';
COMMENT ON COLUMN document_versions.change_type IS '变更类型: created(创建)/updated(更新)/restored(回滚)/deleted(删除)';
COMMENT ON COLUMN document_versions.confluence_version IS 'Confluence 页面版本号';

-- Indexes
CREATE INDEX IF NOT EXISTS idx_doc_versions_document ON document_versions(document_id);
CREATE INDEX IF NOT EXISTS idx_doc_versions_created ON document_versions(document_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_doc_versions_number ON document_versions(document_id, version_number DESC);
CREATE INDEX IF NOT EXISTS idx_doc_versions_hash ON document_versions(content_hash);
CREATE INDEX IF NOT EXISTS idx_doc_versions_type ON document_versions(change_type);

-- ============================================================
-- 2. Extend documents table
-- ============================================================
ALTER TABLE documents ADD COLUMN IF NOT EXISTS current_version INTEGER DEFAULT 1;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS version_count INTEGER DEFAULT 1;

COMMENT ON COLUMN documents.current_version IS '当前版本号';
COMMENT ON COLUMN documents.version_count IS '总版本数';

-- ============================================================
-- 3. Version Retention Policy Table
-- ============================================================
CREATE TABLE IF NOT EXISTS version_retention_policies (
    policy_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id VARCHAR(255) NOT NULL,
    dataset_id VARCHAR(255) REFERENCES datasets(dataset_id) ON DELETE CASCADE,

    -- Retention policy
    max_versions_per_document INTEGER DEFAULT 50,
    retention_days INTEGER DEFAULT 90,
    keep_first_version BOOLEAN DEFAULT TRUE,
    keep_deleted_versions BOOLEAN DEFAULT TRUE,

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Constraints
    UNIQUE(tenant_id, dataset_id)
);

COMMENT ON TABLE version_retention_policies IS '版本保留策略表：控制版本存储和清理';
COMMENT ON COLUMN version_retention_policies.max_versions_per_document IS '每个文档最多保留的版本数';
COMMENT ON COLUMN version_retention_policies.retention_days IS '版本保留天数';

CREATE INDEX IF NOT EXISTS idx_retention_policies_tenant ON version_retention_policies(tenant_id);
CREATE INDEX IF NOT EXISTS idx_retention_policies_dataset ON version_retention_policies(dataset_id);

-- ============================================================
-- 4. Trigger for updated_at
-- ============================================================
DROP TRIGGER IF EXISTS update_version_retention_policies_updated_at ON version_retention_policies;
CREATE TRIGGER update_version_retention_policies_updated_at
    BEFORE UPDATE ON version_retention_policies
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- Migration complete
-- ============================================================
