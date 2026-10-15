-- Migration: 021_artifacts
-- Description: Create artifacts table for persistent artifact storage
-- Date: 2025-01-15

-- Artifacts table: stores metadata for all artifacts (images, documents, charts, etc.)
CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id VARCHAR(64) PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,          -- Associated conversation/session
    message_id VARCHAR(64),                     -- Optional: link to specific message
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,

    -- Artifact type and content info
    type VARCHAR(32) NOT NULL,                  -- image, document, chart, code, file
    format VARCHAR(32) NOT NULL,                -- png, pdf, docx, md, csv, json, etc.
    title VARCHAR(255) NOT NULL,
    filename VARCHAR(255) NOT NULL,

    -- Storage info
    storage_key VARCHAR(512) NOT NULL,          -- S3/storage key path
    size_bytes BIGINT NOT NULL DEFAULT 0,
    mime_type VARCHAR(128),

    -- Source info
    source VARCHAR(32) NOT NULL DEFAULT 'ai',   -- ai | user | code_execution

    -- Flexible metadata
    metadata JSONB DEFAULT '{}',

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_tenant_user ON artifacts(tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_created ON artifacts(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_artifacts_type ON artifacts(type);

-- Add comment
COMMENT ON TABLE artifacts IS 'Persistent storage for conversation artifacts (images, documents, charts, files)';
COMMENT ON COLUMN artifacts.type IS 'Artifact type: image, document, chart, code, file';
COMMENT ON COLUMN artifacts.source IS 'Source of artifact: ai (generated), user (uploaded), code_execution';
COMMENT ON COLUMN artifacts.storage_key IS 'S3/storage key path for the artifact file';
