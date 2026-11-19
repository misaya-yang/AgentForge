-- 013_page_level_sync.sql
-- 添加页面级别的同步配置
-- 允许每个页面独立配置同步策略（覆盖绑定级别的默认设置）

-- 在 confluence_pages 表添加同步配置字段
ALTER TABLE confluence_pages
ADD COLUMN IF NOT EXISTS sync_mode VARCHAR(50) DEFAULT NULL,
ADD COLUMN IF NOT EXISTS polling_interval_minutes INTEGER DEFAULT NULL,
ADD COLUMN IF NOT EXISTS sync_enabled BOOLEAN DEFAULT TRUE,
ADD COLUMN IF NOT EXISTS next_sync_at TIMESTAMPTZ DEFAULT NULL,
ADD COLUMN IF NOT EXISTS sync_priority INTEGER DEFAULT 0;

-- 在 confluence_space_bindings 表添加下次同步时间
ALTER TABLE confluence_space_bindings
ADD COLUMN IF NOT EXISTS next_sync_at TIMESTAMPTZ DEFAULT NULL,
ADD COLUMN IF NOT EXISTS sync_enabled BOOLEAN DEFAULT TRUE;

-- 为调度器创建索引，加速查询需要同步的记录
CREATE INDEX IF NOT EXISTS idx_confluence_pages_next_sync
ON confluence_pages(next_sync_at)
WHERE sync_enabled = TRUE AND sync_mode IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_confluence_bindings_next_sync
ON confluence_space_bindings(next_sync_at)
WHERE sync_enabled = TRUE AND sync_mode = 'polling';

-- 添加注释
COMMENT ON COLUMN confluence_pages.sync_mode IS '页面级同步模式: NULL(继承绑定) | manual(手动) | polling(定时)';
COMMENT ON COLUMN confluence_pages.polling_interval_minutes IS '页面级轮询间隔（分钟），NULL表示继承绑定设置';
COMMENT ON COLUMN confluence_pages.sync_enabled IS '是否启用同步，可以单独禁用某些页面';
COMMENT ON COLUMN confluence_pages.next_sync_at IS '下次计划同步时间';
COMMENT ON COLUMN confluence_pages.sync_priority IS '同步优先级，数字越大优先级越高';
COMMENT ON COLUMN confluence_space_bindings.next_sync_at IS '绑定级别的下次计划同步时间';
COMMENT ON COLUMN confluence_space_bindings.sync_enabled IS '是否启用绑定级别的自动同步';
