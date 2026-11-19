-- T4 (docs/plans/rag-upgrade-prd-2026-08.md): durable lossless document IR
-- and version-keyed page cache.  Central-ledger migration only (PRD §9).
--
-- A document may retain several source/parser generations.  Rechunk reads
-- the exact generation key selected by the worker; a parser/config version
-- change writes a new row instead of mutating the old replay receipt.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE TABLE IF NOT EXISTS knowledge.kb_parsing_ir (
    ir_id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          VARCHAR(255) NOT NULL,
    dataset_id         VARCHAR(255) NOT NULL,
    document_id        VARCHAR(255) NOT NULL,
    generation_key     VARCHAR(255) NOT NULL,
    content_hash       VARCHAR(64) NOT NULL,
    schema_version     VARCHAR(32) NOT NULL DEFAULT '1',
    parser_bundle      VARCHAR(64) NOT NULL,
    parser_config_hash VARCHAR(64) NOT NULL,
    cascade_config     JSONB NOT NULL DEFAULT '{}'::jsonb,
    ir                 JSONB NOT NULL,
    stats              JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_kb_parsing_ir_generation_version UNIQUE
        (tenant_id, dataset_id, document_id, generation_key,
         parser_bundle, parser_config_hash),
    CONSTRAINT chk_kb_parsing_ir_document CHECK (jsonb_typeof(ir) = 'object'),
    CONSTRAINT chk_kb_parsing_ir_config CHECK (jsonb_typeof(cascade_config) = 'object'),
    CONSTRAINT chk_kb_parsing_ir_stats CHECK (jsonb_typeof(stats) = 'object'),
    CONSTRAINT fk_kb_parsing_ir_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT fk_kb_parsing_ir_document_dataset
        FOREIGN KEY (document_id, dataset_id)
        REFERENCES documents (document_id, dataset_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_kb_parsing_ir_document_generation
    ON knowledge.kb_parsing_ir
        (tenant_id, dataset_id, document_id, generation_key, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_kb_parsing_ir_content_version
    ON knowledge.kb_parsing_ir
        (content_hash, parser_bundle, parser_config_hash);

CREATE TABLE IF NOT EXISTS knowledge.kb_parsing_page_cache (
    tenant_id          VARCHAR(255) NOT NULL,
    dataset_id         VARCHAR(255) NOT NULL,
    document_id        VARCHAR(255) NOT NULL,
    generation_key     VARCHAR(255) NOT NULL,
    cache_key          VARCHAR(64) NOT NULL,
    content_hash       VARCHAR(64) NOT NULL,
    page_number        INTEGER NOT NULL CHECK (page_number > 0),
    backend            VARCHAR(100) NOT NULL,
    backend_version    VARCHAR(100) NOT NULL,
    parser_config_hash VARCHAR(64) NOT NULL,
    page_ir            JSONB NOT NULL,
    confidence         DOUBLE PRECISION,
    hard_page          BOOLEAN NOT NULL DEFAULT FALSE,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT pk_kb_parsing_page_cache PRIMARY KEY
        (tenant_id, dataset_id, document_id, cache_key),
    CONSTRAINT uq_kb_parsing_page_cache_version UNIQUE
        (tenant_id, dataset_id, document_id, generation_key,
         content_hash, page_number, backend, backend_version,
         parser_config_hash),
    CONSTRAINT chk_kb_parsing_page_cache_ir CHECK
        (jsonb_typeof(page_ir) = 'object'),
    CONSTRAINT fk_kb_parsing_page_cache_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT fk_kb_parsing_page_cache_document_dataset
        FOREIGN KEY (document_id, dataset_id)
        REFERENCES documents (document_id, dataset_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_kb_parsing_page_cache_resume
    ON knowledge.kb_parsing_page_cache
        (tenant_id, dataset_id, document_id, generation_key, page_number);
CREATE INDEX IF NOT EXISTS idx_kb_parsing_page_cache_version
    ON knowledge.kb_parsing_page_cache
        (content_hash, backend, backend_version, parser_config_hash);

COMMENT ON TABLE knowledge.kb_parsing_ir IS
  'T4 immutable-by-version document-generation DocIR; rechunk consumes this row without reparsing source bytes';
COMMENT ON TABLE knowledge.kb_parsing_page_cache IS
  'T4 tenant-scoped page IR cache keyed by page content, backend/config version for crash resume';

COMMIT;
