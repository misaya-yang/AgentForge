-- ============================================================
-- Migration: 007_confluence_binding_root_page
-- Description: Add root_page_id, root_page_title, and tenant_id to confluence_space_bindings
-- Version: 2.3.0
-- ============================================================

SET client_encoding TO 'UTF8';

-- Add tenant_id column to confluence_space_bindings
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS tenant_id VARCHAR(255);

-- Add root_page_id column to confluence_space_bindings
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS root_page_id VARCHAR(255);

-- Add root_page_title column to confluence_space_bindings
ALTER TABLE confluence_space_bindings ADD COLUMN IF NOT EXISTS root_page_title VARCHAR(512);

-- Add comments
COMMENT ON COLUMN confluence_space_bindings.tenant_id IS '租户 ID';
COMMENT ON COLUMN confluence_space_bindings.root_page_id IS '根页面 ID（可选），如果指定则只同步该页面及其子页面';
COMMENT ON COLUMN confluence_space_bindings.root_page_title IS '根页面标题';

-- Create index on tenant_id
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_tenant ON confluence_space_bindings(tenant_id);

-- ============================================================
-- Migration complete
-- ============================================================
