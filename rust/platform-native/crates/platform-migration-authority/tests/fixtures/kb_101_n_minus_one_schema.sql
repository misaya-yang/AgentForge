
CREATE TABLE datasets (
    dataset_id VARCHAR(255) PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL DEFAULT ''
);
CREATE TABLE dataset_process_rules (
    id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
    rules JSONB NOT NULL DEFAULT '{}'
);
CREATE TABLE documents (
    document_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'uploaded',
    error TEXT,
    metadata JSONB NOT NULL DEFAULT '{}',
    process_rule_id VARCHAR(255),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
CREATE TABLE segments (
    segment_id VARCHAR(255) PRIMARY KEY,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL,
    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
    status VARCHAR(50) DEFAULT 'completed',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    content_hash VARCHAR(64),
    index_node_id VARCHAR(255),
    index_node_hash VARCHAR(255),
    UNIQUE(document_id, position)
);
INSERT INTO datasets (dataset_id, tenant_id) VALUES ('dataset-a', 'tenant-a');
INSERT INTO dataset_process_rules (id, dataset_id) VALUES ('rule-a', 'dataset-a');
INSERT INTO documents (
    document_id, dataset_id, title, status, process_rule_id
) VALUES ('document-a', 'dataset-a', 'Document A', 'completed', 'rule-a');
INSERT INTO segments (
    segment_id, dataset_id, document_id, position, text, content_type
) VALUES ('segment-a', 'dataset-a', 'document-a', 0, 'original', 'text');
