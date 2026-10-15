-- 012_binding_sync_mode.sql
-- 将 sync_mode 从 Connection 级别移动到 Binding 级别
-- 允许每个空间独立配置同步策略

-- 在 binding 级别添加同步配置字段
ALTER TABLE confluence_space_bindings
ADD COLUMN IF NOT EXISTS sync_mode VARCHAR(50) NOT NULL DEFAULT 'manual',
ADD COLUMN IF NOT EXISTS polling_interval_minutes INTEGER DEFAULT 60,
ADD COLUMN IF NOT EXISTS last_incremental_sync_at TIMESTAMPTZ;

-- 从现有 connection 迁移 sync_mode 到 bindings
UPDATE confluence_space_bindings b
SET
    sync_mode = COALESCE(c.sync_mode, 'manual'),
    polling_interval_minutes = COALESCE(c.polling_interval_minutes, 60)
FROM confluence_connections c
WHERE b.connection_id = c.connection_id
  AND b.sync_mode = 'manual';  -- 只更新还是默认值的记录

-- 添加约束
ALTER TABLE confluence_space_bindings
DROP CONSTRAINT IF EXISTS chk_binding_sync_mode,
DROP CONSTRAINT IF EXISTS chk_binding_polling_interval;

ALTER TABLE confluence_space_bindings
ADD CONSTRAINT chk_binding_sync_mode CHECK (sync_mode IN ('manual', 'polling')),
ADD CONSTRAINT chk_binding_polling_interval CHECK (polling_interval_minutes BETWEEN 5 AND 1440);

-- 添加注释
COMMENT ON COLUMN confluence_space_bindings.sync_mode IS '同步模式: manual(手动) | polling(定时轮询)';
COMMENT ON COLUMN confluence_space_bindings.polling_interval_minutes IS '轮询间隔（分钟），范围 5-1440';
COMMENT ON COLUMN confluence_space_bindings.last_incremental_sync_at IS '上次增量同步时间';
