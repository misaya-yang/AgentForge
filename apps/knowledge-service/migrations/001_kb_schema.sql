-- ============================================================
-- Knowledge Base Service - Schema
-- Extracted from gateway database/schema.sql
-- Contains all KB-related tables, indexes, triggers, and functions
-- ============================================================

SET client_encoding TO 'UTF8';

-- Required extensions
DO $$
BEGIN
    BEGIN
        EXECUTE 'CREATE EXTENSION IF NOT EXISTS "uuid-ossp"';
    EXCEPTION WHEN insufficient_privilege THEN
        RAISE NOTICE 'skip extension uuid-ossp: insufficient privilege';
    END;
    BEGIN
        EXECUTE 'CREATE EXTENSION IF NOT EXISTS "pgcrypto"';
    EXCEPTION WHEN insufficient_privilege THEN
        RAISE NOTICE 'skip extension pgcrypto: insufficient privilege';
    END;
END
$$;

-- ============================================================
-- 1. datasets (Knowledge Bases)
-- ============================================================
CREATE TABLE IF NOT EXISTS datasets (
    dataset_id VARCHAR(255) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
    visibility VARCHAR(50) NOT NULL DEFAULT 'private', -- private|tenant|public
    embedding_provider VARCHAR(50) NOT NULL DEFAULT 'gemini',
    embedding_model VARCHAR(100) NOT NULL DEFAULT 'gemini-embedding-001',
    embedding_dimension INTEGER NOT NULL DEFAULT 1024 CHECK (embedding_dimension > 0),
    embedding_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    needs_reindex BOOLEAN NOT NULL DEFAULT FALSE,
    index_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    collection_name VARCHAR(255),
    -- KBMS enhancement (from 002_kbms_enhancements)
    kb_type VARCHAR(50) NOT NULL DEFAULT 'document',
    -- Soft delete fields
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at TIMESTAMPTZ,
    deleted_by VARCHAR(255),
    delete_reason TEXT,
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE datasets IS 'Knowledge base datasets';

-- ============================================================
-- 2. documents
-- ============================================================
CREATE TABLE IF NOT EXISTS documents (
    document_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    source_type VARCHAR(50) NOT NULL DEFAULT 'upload', -- upload|text|url
    source_uri TEXT,
    mime_type VARCHAR(100),
    size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    status VARCHAR(50) NOT NULL DEFAULT 'uploaded', -- uploaded|parsing|segmenting|embedding|completed|failed
    progress NUMERIC(5,2) NOT NULL DEFAULT 0 CHECK (progress >= 0 AND progress <= 100),
    error TEXT,
    content TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Enable/disable fields (from 002_kbms_enhancements)
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    disabled_at TIMESTAMPTZ,
    disabled_by VARCHAR(255),
    -- Archive fields (from 002_kbms_enhancements)
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    archived_reason VARCHAR(255),
    archived_by VARCHAR(255),
    archived_at TIMESTAMPTZ,
    -- Processing fields (from 002_kbms_enhancements)
    process_rule_id VARCHAR(255),
    word_count INTEGER DEFAULT 0,
    segment_count INTEGER DEFAULT 0,
    tokens INTEGER DEFAULT 0,
    batch VARCHAR(255),
    doc_type VARCHAR(50),
    doc_form VARCHAR(50) DEFAULT 'text_model',
    doc_language VARCHAR(50),
    created_by VARCHAR(255),
    -- Confluence fields (from 004_confluence_integration)
    confluence_page_id VARCHAR(255),
    confluence_binding_id VARCHAR(255),
    confluence_version INTEGER,
    confluence_web_url TEXT,
    -- Version control fields (from 025_document_version_control)
    current_version INTEGER DEFAULT 1,
    version_count INTEGER DEFAULT 1,
    -- Hierarchical segments detection result (from 031_hierarchical_segments)
    detection_result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

COMMENT ON TABLE documents IS 'Documents within knowledge base datasets';

-- ============================================================
-- 3. segments
-- ============================================================
CREATE TABLE IF NOT EXISTS segments (
    segment_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0 CHECK (position >= 0),
    text TEXT NOT NULL,
    token_count INTEGER NOT NULL DEFAULT 0 CHECK (token_count >= 0),
    vector_id VARCHAR(255),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Image segment fields (from 010_image_segments)
    content_type VARCHAR(50) NOT NULL DEFAULT 'text',  -- text | image
    image_url TEXT,
    image_attachment_id VARCHAR(100),
    image_filename VARCHAR(255),
    image_media_type VARCHAR(100),
    image_file_size INTEGER,
    -- Multimodal chunk fields (from 014_multimodal_chunks)
    has_images BOOLEAN NOT NULL DEFAULT FALSE,
    image_count INTEGER NOT NULL DEFAULT 0 CHECK (image_count >= 0),
    vlm_description TEXT,
    -- Source traceability fields
    source_type VARCHAR(50) DEFAULT 'unknown',
    source_reference JSONB NOT NULL DEFAULT '{}'::jsonb,
    citation_text VARCHAR(500) DEFAULT '',
    page_number INTEGER,
    section_header VARCHAR(500) DEFAULT '',
    language VARCHAR(10) DEFAULT 'en',
    contextual_prefix TEXT DEFAULT '',
    -- Incremental update detection (from 023_segment_content_hash)
    content_hash VARCHAR(64),
    -- Full-text search (from 028_segments_fulltext_search)
    text_search tsvector,
    -- Hierarchical segment fields (from 031_hierarchical_segments)
    level INTEGER DEFAULT 3,
    parent_segment_id VARCHAR(255),
    summary TEXT,
    page_start INTEGER,
    page_end INTEGER,
    -- KBMS enhancement fields (from 002_kbms_enhancements)
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    disabled_at TIMESTAMPTZ,
    disabled_by VARCHAR(255),
    status VARCHAR(50) DEFAULT 'completed',
    hit_count INTEGER NOT NULL DEFAULT 0,
    word_count INTEGER DEFAULT 0,
    keywords JSONB DEFAULT '[]',
    answer TEXT,
    index_node_id VARCHAR(255),
    index_node_hash VARCHAR(255),
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(document_id, position)
);

COMMENT ON TABLE segments IS 'Text/image segments within documents';

-- Constraints for hierarchical segments
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_segments_level'
    ) THEN
        ALTER TABLE segments ADD CONSTRAINT chk_segments_level CHECK (level IN (1, 2, 3));
    END IF;
END$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'fk_segments_parent'
    ) THEN
        ALTER TABLE segments
            ADD CONSTRAINT fk_segments_parent
            FOREIGN KEY (parent_segment_id)
            REFERENCES segments(segment_id)
            ON DELETE SET NULL;
    END IF;
END$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_segments_page_range'
    ) THEN
        ALTER TABLE segments
            ADD CONSTRAINT chk_segments_page_range
            CHECK (page_start IS NULL OR page_end IS NULL OR page_start <= page_end);
    END IF;
END$$;

-- ============================================================
-- 4. segment_images (Image-Chunk Association)
-- ============================================================
CREATE TABLE IF NOT EXISTS segment_images (
    id BIGSERIAL PRIMARY KEY,
    segment_id VARCHAR(255) NOT NULL REFERENCES segments(segment_id) ON DELETE CASCADE,
    image_segment_id VARCHAR(255) NOT NULL REFERENCES segments(segment_id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    proximity_score FLOAT NOT NULL DEFAULT 1.0 CHECK (proximity_score >= 0 AND proximity_score <= 1),
    char_offset INTEGER DEFAULT 0,
    page_number INTEGER DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(segment_id, image_segment_id)
);

COMMENT ON TABLE segment_images IS 'Image-chunk association for multimodal retrieval';

-- ============================================================
-- 5. dataset_permissions
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_permissions (
    id BIGSERIAL PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    subject_type VARCHAR(50) NOT NULL, -- user|role
    subject_id VARCHAR(255) NOT NULL,
    permission VARCHAR(50) NOT NULL DEFAULT 'viewer', -- owner|editor|viewer
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(dataset_id, subject_type, subject_id)
);

COMMENT ON TABLE dataset_permissions IS 'Dataset access permissions';

-- ============================================================
-- 6. child_chunks (Parent-Child Retrieval)
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

COMMENT ON TABLE child_chunks IS 'Child chunks for parent-child retrieval strategy';

-- ============================================================
-- 7. dataset_keyword_tables
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_keyword_tables (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL UNIQUE REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    keyword_table TEXT NOT NULL DEFAULT '{}',
    data_source_type VARCHAR(50) NOT NULL DEFAULT 'database',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_keyword_tables IS 'BM25 keyword retrieval indexes';

-- ============================================================
-- 8. dataset_process_rules
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_process_rules (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
    rules JSONB NOT NULL DEFAULT '{}',
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_process_rules IS 'Chunking strategy and preprocessing config';

-- ============================================================
-- 9. dataset_queries
-- ============================================================
CREATE TABLE IF NOT EXISTS dataset_queries (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    source VARCHAR(50) NOT NULL DEFAULT 'api',
    source_app_id VARCHAR(255),
    created_by_role VARCHAR(50),
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_queries IS 'Retrieval request logs';

-- ============================================================
-- 10. document_versions
-- ============================================================
CREATE TABLE IF NOT EXISTS document_versions (
    version_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    version_number INTEGER NOT NULL,
    content TEXT NOT NULL,
    content_hash VARCHAR(64) NOT NULL,
    confluence_version INTEGER,
    confluence_updated_at TIMESTAMPTZ,
    title VARCHAR(512),
    metadata JSONB DEFAULT '{}',
    word_count INTEGER DEFAULT 0,
    change_type VARCHAR(50) NOT NULL,
    change_reason TEXT,
    changed_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (document_id, version_number)
);

COMMENT ON TABLE document_versions IS 'Document content version history';

-- ============================================================
-- 11. version_retention_policies
-- ============================================================
CREATE TABLE IF NOT EXISTS version_retention_policies (
    policy_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id VARCHAR(255) NOT NULL,
    dataset_id VARCHAR(255) REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    max_versions_per_document INTEGER DEFAULT 50,
    retention_days INTEGER DEFAULT 90,
    keep_first_version BOOLEAN DEFAULT TRUE,
    keep_deleted_versions BOOLEAN DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, dataset_id)
);

COMMENT ON TABLE version_retention_policies IS 'Auto-cleanup config for old document versions';

-- ============================================================
-- Indexes
-- ============================================================

-- datasets indexes
CREATE INDEX IF NOT EXISTS idx_datasets_tenant_id ON datasets(tenant_id);
CREATE INDEX IF NOT EXISTS idx_datasets_visibility ON datasets(visibility);
CREATE INDEX IF NOT EXISTS idx_datasets_active_created_at ON datasets(created_at DESC) WHERE is_deleted = FALSE;
CREATE INDEX IF NOT EXISTS idx_datasets_active_tenant_visibility ON datasets(tenant_id, visibility, created_at DESC) WHERE is_deleted = FALSE;
CREATE INDEX IF NOT EXISTS idx_datasets_kb_type ON datasets(kb_type);
CREATE INDEX IF NOT EXISTS idx_datasets_tenant_kb_type ON datasets(tenant_id, kb_type);

-- documents indexes
CREATE INDEX IF NOT EXISTS idx_documents_dataset_id ON documents(dataset_id);
CREATE INDEX IF NOT EXISTS idx_documents_status ON documents(status);
CREATE INDEX IF NOT EXISTS idx_documents_created_at ON documents(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_documents_enabled ON documents(enabled);
CREATE INDEX IF NOT EXISTS idx_documents_archived ON documents(archived);
CREATE INDEX IF NOT EXISTS idx_documents_batch ON documents(batch);
CREATE INDEX IF NOT EXISTS idx_documents_confluence_page ON documents(confluence_page_id) WHERE confluence_page_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_documents_confluence_binding ON documents(confluence_binding_id) WHERE confluence_binding_id IS NOT NULL;

-- segments indexes
CREATE INDEX IF NOT EXISTS idx_segments_dataset_id ON segments(dataset_id);
CREATE INDEX IF NOT EXISTS idx_segments_document_id ON segments(document_id);
CREATE INDEX IF NOT EXISTS idx_segments_vector_id ON segments(vector_id);
CREATE INDEX IF NOT EXISTS idx_segments_content_hash ON segments(document_id, content_hash);
CREATE INDEX IF NOT EXISTS idx_segments_content_type ON segments(content_type);
CREATE INDEX IF NOT EXISTS idx_segments_has_images ON segments(has_images) WHERE has_images = TRUE;
CREATE INDEX IF NOT EXISTS idx_segments_text_search ON segments USING GIN (text_search);
CREATE INDEX IF NOT EXISTS idx_segments_source_type ON segments(source_type);
CREATE INDEX IF NOT EXISTS idx_segments_language ON segments(language);
CREATE INDEX IF NOT EXISTS idx_segments_enabled ON segments(enabled);
CREATE INDEX IF NOT EXISTS idx_segments_status ON segments(status);
CREATE INDEX IF NOT EXISTS idx_segments_index_node_id ON segments(index_node_id);
CREATE INDEX IF NOT EXISTS idx_segments_image_attachment ON segments(image_attachment_id) WHERE image_attachment_id IS NOT NULL;

-- segment_images indexes
CREATE INDEX IF NOT EXISTS idx_segment_images_segment ON segment_images(segment_id);
CREATE INDEX IF NOT EXISTS idx_segment_images_image ON segment_images(image_segment_id);
CREATE INDEX IF NOT EXISTS idx_segment_images_proximity ON segment_images(segment_id, proximity_score DESC);

-- dataset_permissions indexes
CREATE INDEX IF NOT EXISTS idx_dataset_permissions_dataset_id ON dataset_permissions(dataset_id);
CREATE INDEX IF NOT EXISTS idx_dataset_permissions_subject ON dataset_permissions(subject_type, subject_id);

-- child_chunks indexes
CREATE INDEX IF NOT EXISTS idx_child_chunks_dataset_id ON child_chunks(dataset_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_document_id ON child_chunks(document_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_segment_id ON child_chunks(segment_id);
CREATE INDEX IF NOT EXISTS idx_child_chunks_index_node_id ON child_chunks(index_node_id);

-- dataset_process_rules indexes
CREATE INDEX IF NOT EXISTS idx_process_rules_dataset_id ON dataset_process_rules(dataset_id);

-- dataset_queries indexes
CREATE INDEX IF NOT EXISTS idx_dataset_queries_dataset_id ON dataset_queries(dataset_id);
CREATE INDEX IF NOT EXISTS idx_dataset_queries_created_at ON dataset_queries(created_at DESC);

-- document_versions indexes
CREATE INDEX IF NOT EXISTS idx_doc_versions_document ON document_versions(document_id);
CREATE INDEX IF NOT EXISTS idx_doc_versions_number ON document_versions(document_id, version_number DESC);
CREATE INDEX IF NOT EXISTS idx_doc_versions_hash ON document_versions(content_hash);
CREATE INDEX IF NOT EXISTS idx_doc_versions_type ON document_versions(change_type);
CREATE INDEX IF NOT EXISTS idx_doc_versions_created ON document_versions(document_id, created_at DESC);

-- version_retention_policies indexes
CREATE INDEX IF NOT EXISTS idx_retention_policies_tenant ON version_retention_policies(tenant_id);
CREATE INDEX IF NOT EXISTS idx_retention_policies_dataset ON version_retention_policies(dataset_id);

-- ============================================================
-- Functions and Triggers
-- ============================================================

-- Auto-update updated_at
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Auto-populate text_search tsvector
CREATE OR REPLACE FUNCTION segments_text_search_update() RETURNS trigger AS $$
BEGIN
    NEW.text_search := to_tsvector('simple', COALESCE(NEW.text, ''));
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- updated_at triggers
DROP TRIGGER IF EXISTS update_datasets_updated_at ON datasets;
CREATE TRIGGER update_datasets_updated_at BEFORE UPDATE ON datasets
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_documents_updated_at ON documents;
CREATE TRIGGER update_documents_updated_at BEFORE UPDATE ON documents
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_segments_updated_at ON segments;
CREATE TRIGGER update_segments_updated_at BEFORE UPDATE ON segments
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_dataset_permissions_updated_at ON dataset_permissions;
CREATE TRIGGER update_dataset_permissions_updated_at BEFORE UPDATE ON dataset_permissions
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- text_search trigger
DROP TRIGGER IF EXISTS trg_segments_text_search ON segments;
CREATE TRIGGER trg_segments_text_search
    BEFORE INSERT OR UPDATE OF text ON segments
    FOR EACH ROW
    EXECUTE FUNCTION segments_text_search_update();
