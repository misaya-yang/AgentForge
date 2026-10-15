-- ============================================================
-- Migration: 011_confluence_acl
-- Description: Add owner_id for ACL enforcement on Confluence resources
-- Version: 2.3.0
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. 添加 owner_id 到 confluence_connections 表
-- ============================================================
ALTER TABLE confluence_connections
    ADD COLUMN IF NOT EXISTS owner_id VARCHAR(255);

-- 回填：如果 owner_id 为空，使用 created_by
UPDATE confluence_connections
SET owner_id = created_by
WHERE owner_id IS NULL AND created_by IS NOT NULL;

-- 添加索引
CREATE INDEX IF NOT EXISTS idx_confluence_connections_owner
    ON confluence_connections(owner_id);

COMMENT ON COLUMN confluence_connections.owner_id IS '资源所有者 ID，用于 ACL 控制';

-- ============================================================
-- 2. 添加 owner_id 到 confluence_space_bindings 表
-- ============================================================
ALTER TABLE confluence_space_bindings
    ADD COLUMN IF NOT EXISTS owner_id VARCHAR(255);

-- 回填：如果 owner_id 为空，使用 created_by
UPDATE confluence_space_bindings
SET owner_id = created_by
WHERE owner_id IS NULL AND created_by IS NOT NULL;

-- 添加索引
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_owner
    ON confluence_space_bindings(owner_id);

COMMENT ON COLUMN confluence_space_bindings.owner_id IS '资源所有者 ID，用于 ACL 控制';

-- ============================================================
-- 3. 添加 owner_id 到 confluence_sync_tasks 表
-- ============================================================
ALTER TABLE confluence_sync_tasks
    ADD COLUMN IF NOT EXISTS owner_id VARCHAR(255);

-- 回填：从关联的 binding 获取 owner_id
UPDATE confluence_sync_tasks t
SET owner_id = b.owner_id
FROM confluence_space_bindings b
WHERE t.binding_id = b.binding_id
  AND t.owner_id IS NULL
  AND b.owner_id IS NOT NULL;

-- 添加索引
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_owner
    ON confluence_sync_tasks(owner_id);

COMMENT ON COLUMN confluence_sync_tasks.owner_id IS '资源所有者 ID，用于 ACL 控制';

-- ============================================================
-- Migration complete
-- ============================================================
