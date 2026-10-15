-- ============================================================
-- Migration: 004_confluence_integration
-- Description: Add Confluence integration tables for KBMS
-- Version: 2.2.0
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. Confluence 连接配置表
-- ============================================================
CREATE TABLE IF NOT EXISTS confluence_connections (
    connection_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
    name VARCHAR(255) NOT NULL,
    domain VARCHAR(255) NOT NULL,  -- e.g., 'yourcompany.atlassian.net'
    email VARCHAR(255) NOT NULL,
    api_token TEXT NOT NULL,  -- API Token (明文存储，后期加密)
    base_url VARCHAR(512),  -- 完整 base URL (如果非标准)

    -- 同步配置
    sync_mode VARCHAR(50) NOT NULL DEFAULT 'manual',  -- manual | polling
    polling_interval_minutes INTEGER DEFAULT 60,

    -- 状态
    status VARCHAR(50) NOT NULL DEFAULT 'active',  -- active | disabled | error
    last_sync_at TIMESTAMPTZ,
    last_error TEXT,

    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE confluence_connections IS 'Confluence 连接配置表：存储认证信息和同步配置';
COMMENT ON COLUMN confluence_connections.domain IS 'Atlassian 域名，如 yourcompany.atlassian.net';
COMMENT ON COLUMN confluence_connections.api_token IS 'API Token (明文存储，后期加密)';
COMMENT ON COLUMN confluence_connections.sync_mode IS '同步模式: manual(手动)/polling(轮询)';

CREATE INDEX IF NOT EXISTS idx_confluence_connections_tenant ON confluence_connections(tenant_id);
CREATE INDEX IF NOT EXISTS idx_confluence_connections_status ON confluence_connections(status);
CREATE INDEX IF NOT EXISTS idx_confluence_connections_domain ON confluence_connections(domain);

-- ============================================================
-- 2. Confluence 空间绑定表
-- ============================================================
CREATE TABLE IF NOT EXISTS confluence_space_bindings (
    binding_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    connection_id VARCHAR(255) NOT NULL REFERENCES confluence_connections(connection_id) ON DELETE CASCADE,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    space_key VARCHAR(255) NOT NULL,
    space_id VARCHAR(255),  -- Confluence 内部 Space ID
    space_name VARCHAR(255),

    -- 过滤配置
    include_patterns JSONB DEFAULT '[]'::jsonb,  -- 页面标题/标签匹配规则
    exclude_patterns JSONB DEFAULT '[]'::jsonb,
    max_depth INTEGER DEFAULT 10,  -- 页面层级深度限制
    include_attachments BOOLEAN DEFAULT FALSE,
    include_comments BOOLEAN DEFAULT FALSE,

    -- 同步状态
    status VARCHAR(50) NOT NULL DEFAULT 'pending',  -- pending | syncing | completed | error
    last_sync_at TIMESTAMPTZ,
    synced_page_count INTEGER DEFAULT 0,
    total_page_count INTEGER DEFAULT 0,
    last_error TEXT,

    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(connection_id, space_key, dataset_id)
);

COMMENT ON TABLE confluence_space_bindings IS 'Confluence 空间与知识库绑定关系表';
COMMENT ON COLUMN confluence_space_bindings.space_key IS 'Confluence Space Key (短标识)';
COMMENT ON COLUMN confluence_space_bindings.include_patterns IS '包含规则，JSON 数组格式';

CREATE INDEX IF NOT EXISTS idx_confluence_bindings_connection ON confluence_space_bindings(connection_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_dataset ON confluence_space_bindings(dataset_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_space ON confluence_space_bindings(space_key);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_status ON confluence_space_bindings(status);

-- ============================================================
-- 3. Confluence 页面同步记录表
-- ============================================================
CREATE TABLE IF NOT EXISTS confluence_pages (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    binding_id VARCHAR(255) NOT NULL REFERENCES confluence_space_bindings(binding_id) ON DELETE CASCADE,
    document_id VARCHAR(255) REFERENCES documents(document_id) ON DELETE SET NULL,

    -- Confluence 页面信息
    page_id VARCHAR(255) NOT NULL,  -- Confluence Page ID
    space_key VARCHAR(255) NOT NULL,
    title VARCHAR(512) NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    content_hash VARCHAR(64),  -- SHA256 of page content for change detection

    -- 父子关系
    parent_page_id VARCHAR(255),
    depth INTEGER DEFAULT 0,

    -- 同步状态
    status VARCHAR(50) NOT NULL DEFAULT 'pending',  -- pending | synced | error | deleted
    last_synced_at TIMESTAMPTZ,
    confluence_updated_at TIMESTAMPTZ,  -- Confluence 端的更新时间
    error TEXT,

    -- 元数据
    labels JSONB DEFAULT '[]'::jsonb,
    web_url TEXT,
    author VARCHAR(255),

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(binding_id, page_id)
);

COMMENT ON TABLE confluence_pages IS 'Confluence 页面同步记录表：跟踪已同步的页面状态';
COMMENT ON COLUMN confluence_pages.content_hash IS '页面内容哈希，用于变更检测';
COMMENT ON COLUMN confluence_pages.version IS 'Confluence 页面版本号';

CREATE INDEX IF NOT EXISTS idx_confluence_pages_binding ON confluence_pages(binding_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_document ON confluence_pages(document_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_page_id ON confluence_pages(page_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_status ON confluence_pages(status);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_parent ON confluence_pages(parent_page_id);

-- ============================================================
-- 4. Confluence Webhook 配置表 (二期使用)
-- ============================================================
CREATE TABLE IF NOT EXISTS confluence_webhooks (
    webhook_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    connection_id VARCHAR(255) NOT NULL REFERENCES confluence_connections(connection_id) ON DELETE CASCADE,

    -- Confluence 侧注册信息
    confluence_webhook_id VARCHAR(255),  -- Confluence 返回的 Webhook ID
    webhook_secret VARCHAR(255) NOT NULL,  -- 用于验证 Webhook 签名
    callback_url TEXT NOT NULL,

    -- 事件订阅
    events JSONB NOT NULL DEFAULT '["page_created", "page_updated", "page_removed"]'::jsonb,

    -- 状态
    status VARCHAR(50) NOT NULL DEFAULT 'pending',  -- pending | active | error | disabled
    last_event_at TIMESTAMPTZ,
    error TEXT,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE confluence_webhooks IS 'Confluence Webhook 配置表：管理 Webhook 注册和事件订阅 (二期使用)';

CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_connection ON confluence_webhooks(connection_id);
CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_status ON confluence_webhooks(status);
CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_secret ON confluence_webhooks(webhook_secret);

-- ============================================================
-- 5. Confluence 同步任务队列表
-- ============================================================
CREATE TABLE IF NOT EXISTS confluence_sync_tasks (
    task_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    binding_id VARCHAR(255) REFERENCES confluence_space_bindings(binding_id) ON DELETE CASCADE,
    page_id VARCHAR(255),  -- NULL 表示空间全量同步

    task_type VARCHAR(50) NOT NULL,  -- full_sync | incremental_sync | page_sync | page_delete
    priority INTEGER DEFAULT 0,

    status VARCHAR(50) NOT NULL DEFAULT 'pending',  -- pending | processing | completed | failed
    retry_count INTEGER DEFAULT 0,
    max_retries INTEGER DEFAULT 3,

    -- 进度信息
    progress NUMERIC(5,2) DEFAULT 0,
    total_items INTEGER DEFAULT 0,
    processed_items INTEGER DEFAULT 0,

    error TEXT,
    result JSONB,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

COMMENT ON TABLE confluence_sync_tasks IS 'Confluence 同步任务队列表：异步处理同步任务';

CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_status ON confluence_sync_tasks(status, priority DESC);
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_binding ON confluence_sync_tasks(binding_id);
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_created ON confluence_sync_tasks(created_at DESC);

-- ============================================================
-- 6. 扩展 documents 表
-- ============================================================
ALTER TABLE documents ADD COLUMN IF NOT EXISTS confluence_page_id VARCHAR(255);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS confluence_binding_id VARCHAR(255);
ALTER TABLE documents ADD COLUMN IF NOT EXISTS confluence_version INTEGER;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS confluence_web_url TEXT;

COMMENT ON COLUMN documents.confluence_page_id IS 'Confluence 页面 ID (如果文档来源是 Confluence)';
COMMENT ON COLUMN documents.confluence_binding_id IS 'Confluence 空间绑定 ID';
COMMENT ON COLUMN documents.confluence_version IS 'Confluence 页面版本号';
COMMENT ON COLUMN documents.confluence_web_url IS 'Confluence 页面 Web URL';

CREATE INDEX IF NOT EXISTS idx_documents_confluence_page ON documents(confluence_page_id) WHERE confluence_page_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_documents_confluence_binding ON documents(confluence_binding_id) WHERE confluence_binding_id IS NOT NULL;

-- ============================================================
-- 7. 触发器
-- ============================================================
DROP TRIGGER IF EXISTS update_confluence_connections_updated_at ON confluence_connections;
CREATE TRIGGER update_confluence_connections_updated_at
    BEFORE UPDATE ON confluence_connections
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_space_bindings_updated_at ON confluence_space_bindings;
CREATE TRIGGER update_confluence_space_bindings_updated_at
    BEFORE UPDATE ON confluence_space_bindings
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_pages_updated_at ON confluence_pages;
CREATE TRIGGER update_confluence_pages_updated_at
    BEFORE UPDATE ON confluence_pages
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_webhooks_updated_at ON confluence_webhooks;
CREATE TRIGGER update_confluence_webhooks_updated_at
    BEFORE UPDATE ON confluence_webhooks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_sync_tasks_updated_at ON confluence_sync_tasks;
CREATE TRIGGER update_confluence_sync_tasks_updated_at
    BEFORE UPDATE ON confluence_sync_tasks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- Migration complete
-- ============================================================
