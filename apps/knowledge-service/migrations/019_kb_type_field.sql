-- Migration: 019_kb_type_field.sql
-- Description: Add kb_type and use_case fields to datasets for multimodal KB support
-- Date: 2026-01-15

-- Add kb_type field: document | data | image | audio_video
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS kb_type VARCHAR(20) NOT NULL DEFAULT 'document';

-- Add use_case field: basic_qa | rich_text_response
ALTER TABLE datasets ADD COLUMN IF NOT EXISTS use_case VARCHAR(50) NOT NULL DEFAULT 'basic_qa';

-- Create index for type filtering
CREATE INDEX IF NOT EXISTS idx_datasets_kb_type ON datasets (kb_type);
CREATE INDEX IF NOT EXISTS idx_datasets_tenant_kb_type ON datasets (tenant_id, kb_type);

-- Add comment for documentation
COMMENT ON COLUMN datasets.kb_type IS 'Knowledge base type: document (文档搜索), data (数据查询), image (图片问答), audio_video (音视频搜索)';
COMMENT ON COLUMN datasets.use_case IS 'Use case scenario: basic_qa (基础文档问答), rich_text_response (图文并茂回复)';
