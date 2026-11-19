-- ============================================================
-- Agent Gateway KBMS Enhancement Migration
-- Version: 2.1.0
-- Description: Add Dify-style features to knowledge base management
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. Dataset Process Rules Table (类似 Dify DatasetProcessRule)
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_process_rules (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    mode VARCHAR(50) NOT NULL DEFAULT 'automatic', -- automatic|custom|hierarchical
    rules JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_process_rules IS '数据集处理规则表：定义文档分段、清洗的规则配置';
COMMENT ON COLUMN dataset_process_rules.mode IS '处理模式: automatic(自动), custom(自定义), hierarchical(层级)';
COMMENT ON COLUMN dataset_process_rules.rules IS '规则配置JSON，包含pre_processing_rules和segmentation设置';

CREATE INDEX IF NOT EXISTS idx_process_rules_dataset_id ON dataset_process_rules(dataset_id);

-- ============================================================
-- 2. Enhance datasets table
-- ============================================================
-- Add indexing_technique column
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS indexing_technique VARCHAR(50) DEFAULT 'high_quality';
COMMENT ON COLUMN datasets.indexing_technique IS '索引技术: high_quality(高质量向量), economy(经济模式关键词)';

-- Add statistics columns
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS document_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS segment_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS word_count BIGINT NOT NULL DEFAULT 0;

-- Add icon/UI related fields
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS icon_info JSONB DEFAULT '{}'::jsonb;

-- ============================================================
-- 3. Enhance documents table
-- ============================================================
-- Add enabled/disabled support
ALTER TABLE documents ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS disabled_by VARCHAR(255);

-- Add archived support  
ALTER TABLE documents ADD COLUMN IF NOT EXISTS archived BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS archived_reason VARCHAR(255);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS archived_by VARCHAR(255);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

-- Add processing rule reference
ALTER TABLE documents ADD COLUMN IF NOT EXISTS process_rule_id VARCHAR(255);

-- Add word count and segment count
ALTER TABLE documents ADD COLUMN IF NOT EXISTS word_count INTEGER DEFAULT 0;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS segment_count INTEGER DEFAULT 0;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS tokens INTEGER DEFAULT 0;

-- Add batch ID for batch operations
ALTER TABLE documents ADD COLUMN IF NOT EXISTS batch VARCHAR(255);

-- Add doc type and form
ALTER TABLE documents ADD COLUMN IF NOT EXISTS doc_type VARCHAR(50);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS doc_form VARCHAR(50) DEFAULT 'text_model';
ALTER TABLE documents ADD COLUMN IF NOT EXISTS doc_language VARCHAR(50);

-- Add created_by column
ALTER TABLE documents ADD COLUMN IF NOT EXISTS created_by VARCHAR(255);

CREATE INDEX IF NOT EXISTS idx_documents_enabled ON documents(enabled);
CREATE INDEX IF NOT EXISTS idx_documents_archived ON documents(archived);
CREATE INDEX IF NOT EXISTS idx_documents_batch ON documents(batch);

-- ============================================================
-- 4. Enhance segments table
-- ============================================================
-- Add enabled/disabled support
ALTER TABLE segments ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE segments ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ;
ALTER TABLE segments ADD COLUMN IF NOT EXISTS disabled_by VARCHAR(255);

-- Add status for async processing
ALTER TABLE segments ADD COLUMN IF NOT EXISTS status VARCHAR(50) DEFAULT 'completed';

-- Add hit count for analytics
ALTER TABLE segments ADD COLUMN IF NOT EXISTS hit_count INTEGER NOT NULL DEFAULT 0;

-- Add word count
ALTER TABLE segments ADD COLUMN IF NOT EXISTS word_count INTEGER DEFAULT 0;

-- Add keywords for keyword search enhancement
ALTER TABLE segments ADD COLUMN IF NOT EXISTS keywords JSONB DEFAULT '[]'::jsonb;

-- Add answer field for Q&A mode
ALTER TABLE segments ADD COLUMN IF NOT EXISTS answer TEXT;

-- Add index node fields for vector store tracking
ALTER TABLE segments ADD COLUMN IF NOT EXISTS index_node_id VARCHAR(255);
ALTER TABLE segments ADD COLUMN IF NOT EXISTS index_node_hash VARCHAR(255);

-- Add created_by column
ALTER TABLE segments ADD COLUMN IF NOT EXISTS created_by VARCHAR(255);

CREATE INDEX IF NOT EXISTS idx_segments_enabled ON segments(enabled);
CREATE INDEX IF NOT EXISTS idx_segments_status ON segments(status);
CREATE INDEX IF NOT EXISTS idx_segments_index_node_id ON segments(index_node_id);

-- ============================================================
-- 5. Child Chunks Table (for hierarchical chunking)
-- ============================================================
CREATE TABLE IF NOT EXISTS child_chunks (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    segment_id VARCHAR(255) NOT NULL REFERENCES segments(segment_id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    content TEXT NOT NULL,
    word_count INTEGER NOT NULL DEFAULT 0,
    index_node_id VARCHAR(255),
    index_node_hash VARCHAR(255),
    type VARCHAR(50) NOT NULL DEFAULT 'automatic',
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    indexing_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error TEXT
);

COMMENT ON TABLE child_chunks IS '子块表：用于层级分块模式的子块存储';

CREATE INDEX IF NOT EXISTS idx_child_chunks_segment_id ON child_chunks(segment_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_dataset_id ON child_chunks(dataset_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_document_id ON child_chunks(document_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_index_node_id ON child_chunks(index_node_id);

-- ============================================================
-- 6. Dataset Keyword Table (for BM25/keyword search)
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_keyword_tables (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL UNIQUE REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    keyword_table TEXT NOT NULL DEFAULT '{}',
    data_source_type VARCHAR(50) NOT NULL DEFAULT 'database',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_keyword_tables IS '数据集关键词表：用于BM25关键词检索的倒排索引';

-- ============================================================
-- 7. Dataset Queries Table (query history/analytics)
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_queries (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    source VARCHAR(50) NOT NULL DEFAULT 'api', -- api|hit_testing|app
    source_app_id VARCHAR(255),
    created_by_role VARCHAR(50),
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_queries IS '数据集查询历史表：记录检索查询用于分析';

CREATE INDEX IF NOT EXISTS idx_dataset_queries_dataset_id ON dataset_queries(dataset_id);
CREATE INDEX IF NOT EXISTS idx_dataset_queries_created_at ON dataset_queries(created_at DESC);

-- ============================================================
-- 8. Update triggers
-- ============================================================
DROP TRIGGER IF EXISTS update_dataset_process_rules_updated_at ON dataset_process_rules;
CREATE TRIGGER update_dataset_process_rules_updated_at BEFORE UPDATE ON dataset_process_rules
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_child_chunks_updated_at ON child_chunks;
CREATE TRIGGER update_child_chunks_updated_at BEFORE UPDATE ON child_chunks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_dataset_keyword_tables_updated_at ON dataset_keyword_tables;
CREATE TRIGGER update_dataset_keyword_tables_updated_at BEFORE UPDATE ON dataset_keyword_tables
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- 9. Default process rules are created dynamically per dataset
-- ============================================================
-- Note: Default rules are created when a dataset is created, not as a global template.


