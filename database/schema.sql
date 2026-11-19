-- ============================================================
-- Agent Gateway 数据库建表脚本
-- 数据库：PostgreSQL
-- 版本：2.0.0
-- ============================================================

-- This file is the stable bootstrap baseline, not the final runtime shape.
-- A fresh installation must apply the canonical central migration ledger
-- immediately afterwards; baseline + recorded migrations is authoritative.

SET client_encoding TO 'UTF8';

-- 可选扩展（需要足够权限）
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
-- 1. 服务定义表
-- ============================================================
CREATE TABLE IF NOT EXISTS services (
    service_id VARCHAR(255) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    version VARCHAR(50) NOT NULL DEFAULT '1.0.0',
    service_type VARCHAR(50) NOT NULL DEFAULT 'custom',
    connector_type VARCHAR(50) NOT NULL DEFAULT 'http',
    connector_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    supported_modes VARCHAR(50)[] NOT NULL DEFAULT ARRAY['sync', 'stream']::VARCHAR(50)[],
    accepted_content_types VARCHAR(50)[] NOT NULL DEFAULT ARRAY['text']::VARCHAR(50)[],
    output_content_types VARCHAR(50)[] NOT NULL DEFAULT ARRAY['text']::VARCHAR(50)[],
    input_schema JSONB NOT NULL DEFAULT '{}'::jsonb,
    output_schema JSONB NOT NULL DEFAULT '{}'::jsonb,
    session_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    session_adapter VARCHAR(100),
    timeout INTEGER NOT NULL DEFAULT 60 CHECK (timeout > 0),
    max_retries INTEGER NOT NULL DEFAULT 3 CHECK (max_retries >= 0),
    retry_delay NUMERIC(5,2) NOT NULL DEFAULT 1.0 CHECK (retry_delay >= 0),
    circuit_breaker_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    failure_threshold INTEGER NOT NULL DEFAULT 5 CHECK (failure_threshold >= 1),
    recovery_timeout INTEGER NOT NULL DEFAULT 30 CHECK (recovery_timeout >= 0),
    rate_limit JSONB,
    concurrency_limit INTEGER CHECK (concurrency_limit IS NULL OR concurrency_limit >= 0),
    service_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    async_config JSONB,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    tags VARCHAR(100)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(100)[],
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE services IS '服务定义表：描述可被网关调用的 AI/工具服务及其连接与运行参数。';
COMMENT ON COLUMN services.service_id IS '服务唯一标识（业务主键，通常来自配置或注册接口）。';
COMMENT ON COLUMN services.name IS '服务显示名称。';
COMMENT ON COLUMN services.description IS '服务描述。';
COMMENT ON COLUMN services.version IS '服务版本号。';
COMMENT ON COLUMN services.service_type IS '服务类型（如 custom / conversational / text2image 等，取值由业务约定）。';
COMMENT ON COLUMN services.connector_type IS '连接器类型（http/openai/dify/langgraph 等）。';
COMMENT ON COLUMN services.connector_config IS '连接器配置（base_url、headers、模型名等）。';
COMMENT ON COLUMN services.supported_modes IS '支持的调用模式（sync/stream/async）。';
COMMENT ON COLUMN services.accepted_content_types IS '支持的输入内容类型（text/image/audio 等）。';
COMMENT ON COLUMN services.output_content_types IS '输出内容类型（text/image/audio 等）。';
COMMENT ON COLUMN services.input_schema IS '输入 JSON Schema（可选，用于校验/生成 UI）。';
COMMENT ON COLUMN services.output_schema IS '输出 JSON Schema（可选）。';
COMMENT ON COLUMN services.session_enabled IS '是否启用会话能力（session_id）。';
COMMENT ON COLUMN services.session_adapter IS '会话适配器名称（可选）。';
COMMENT ON COLUMN services.timeout IS '请求超时（秒）。';
COMMENT ON COLUMN services.max_retries IS '最大重试次数。';
COMMENT ON COLUMN services.retry_delay IS '重试间隔（秒，支持小数）。';
COMMENT ON COLUMN services.circuit_breaker_enabled IS '是否启用熔断。';
COMMENT ON COLUMN services.failure_threshold IS '触发熔断的失败阈值（次数）。';
COMMENT ON COLUMN services.recovery_timeout IS '熔断后恢复等待时间（秒）。';
COMMENT ON COLUMN services.rate_limit IS '服务级限流配置（兼容字段）。';
COMMENT ON COLUMN services.concurrency_limit IS '并发限制（可选）。';
COMMENT ON COLUMN services.service_config IS '服务综合配置（新版字段：限流/鉴权/缓存/优先级等）。';
COMMENT ON COLUMN services.async_config IS '异步执行配置（回调、队列等）。';
COMMENT ON COLUMN services.status IS '服务状态（active/inactive/disabled 等）。';
COMMENT ON COLUMN services.tags IS '标签数组，用于筛选/分组。';
COMMENT ON COLUMN services.metadata IS '扩展元数据（如 adapter_type、分类等）。';
COMMENT ON COLUMN services.created_at IS '创建时间。';
COMMENT ON COLUMN services.updated_at IS '更新时间（由触发器自动维护）。';

-- ============================================================
-- 2. API Key 表
-- ============================================================
CREATE TABLE IF NOT EXISTS api_keys (
    id SERIAL PRIMARY KEY,
    key_hash VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255),
    description TEXT,
    tenant_id VARCHAR(255),
    user_id VARCHAR(255),
    roles VARCHAR(50)[] NOT NULL DEFAULT ARRAY['user']::VARCHAR(50)[],
    permissions VARCHAR(100)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(100)[],
    tier VARCHAR(50) NOT NULL DEFAULT 'normal',
    rate_limit JSONB,
    allowed_services VARCHAR(255)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(255)[],
    allowed_models VARCHAR(255)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(255)[],
    expires_at TIMESTAMPTZ,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_used_at TIMESTAMPTZ,
    use_count BIGINT NOT NULL DEFAULT 0 CHECK (use_count >= 0),
    -- Key identification (from 003_proxy_enhancements)
    key_id VARCHAR(32),
    key_prefix VARCHAR(12),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE api_keys IS 'API Key 管理表：用于请求鉴权、权限控制、配额/限流策略等。';
COMMENT ON COLUMN api_keys.id IS '自增主键。';
COMMENT ON COLUMN api_keys.key_hash IS 'API Key 哈希（仅存哈希，不存明文）。';
COMMENT ON COLUMN api_keys.name IS 'Key 名称（便于识别）。';
COMMENT ON COLUMN api_keys.description IS 'Key 描述。';
COMMENT ON COLUMN api_keys.tenant_id IS '租户 ID（可选，空表示不限定租户）。';
COMMENT ON COLUMN api_keys.user_id IS '用户 ID（可选）。';
COMMENT ON COLUMN api_keys.roles IS '角色列表（用于 RBAC）。';
COMMENT ON COLUMN api_keys.permissions IS '权限列表（可选，细粒度权限）。';
COMMENT ON COLUMN api_keys.tier IS '用户层级（anonymous/normal/premium/enterprise/admin 等）。';
COMMENT ON COLUMN api_keys.rate_limit IS 'Key 级限流覆盖配置（可选）。';
COMMENT ON COLUMN api_keys.allowed_services IS '允许访问的服务列表（可选）。';
COMMENT ON COLUMN api_keys.allowed_models IS '允许访问的模型白名单（可选）。';
COMMENT ON COLUMN api_keys.expires_at IS '过期时间（可选）。';
COMMENT ON COLUMN api_keys.enabled IS '是否启用。';
COMMENT ON COLUMN api_keys.last_used_at IS '最后使用时间。';
COMMENT ON COLUMN api_keys.use_count IS '使用次数累计。';
COMMENT ON COLUMN api_keys.created_at IS '创建时间。';
COMMENT ON COLUMN api_keys.updated_at IS '更新时间（由触发器自动维护）。';

-- ============================================================
-- 3. 用户表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS users (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL UNIQUE,
    username VARCHAR(255),
    email VARCHAR(255),
    display_name VARCHAR(255),
    tenant_id VARCHAR(255) NOT NULL DEFAULT 'default',
    tier VARCHAR(50) NOT NULL DEFAULT 'normal',
    roles VARCHAR(50)[] NOT NULL DEFAULT ARRAY['user']::VARCHAR(50)[],
    permissions VARCHAR(100)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(100)[],
    quota_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Account security fields (from 005_account_permission_system)
    password_hash VARCHAR(255),
    force_password_change BOOLEAN NOT NULL DEFAULT TRUE,
    password_changed_at TIMESTAMPTZ,
    login_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    last_login_at TIMESTAMPTZ,
    last_login_ip VARCHAR(45),
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_by VARCHAR(255),
    department VARCHAR(50),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_active_at TIMESTAMPTZ
);

COMMENT ON TABLE users IS '用户表（预留）：用于持久化用户画像、权限、配额等（当前项目主要从 JWT/API Key 解析）。';
COMMENT ON COLUMN users.id IS '自增主键。';
COMMENT ON COLUMN users.user_id IS '用户唯一标识（业务主键，通常来自 JWT sub / 外部系统）。';
COMMENT ON COLUMN users.username IS '用户名（可选）。';
COMMENT ON COLUMN users.email IS '邮箱（可选）。';
COMMENT ON COLUMN users.display_name IS '显示名（可选）。';
COMMENT ON COLUMN users.tenant_id IS '所属租户 ID。';
COMMENT ON COLUMN users.tier IS '用户层级（anonymous/normal/premium/enterprise/admin 等）。';
COMMENT ON COLUMN users.roles IS '角色列表。';
COMMENT ON COLUMN users.permissions IS '权限列表（可选）。';
COMMENT ON COLUMN users.quota_config IS '配额配置（如 token/请求数上限等，可选）。';
COMMENT ON COLUMN users.status IS '用户状态（active/disabled 等）。';
COMMENT ON COLUMN users.metadata IS '扩展元数据。';
COMMENT ON COLUMN users.created_at IS '创建时间。';
COMMENT ON COLUMN users.updated_at IS '更新时间（由触发器自动维护）。';
COMMENT ON COLUMN users.last_active_at IS '最后活跃时间。';

-- ============================================================
-- 4. 租户表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS tenants (
    id SERIAL PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    tier VARCHAR(50) NOT NULL DEFAULT 'normal',
    quota_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    rate_limit JSONB,
    allowed_services VARCHAR(255)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(255)[],
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE tenants IS '租户表：用于多租户隔离、配额与限流策略。';
COMMENT ON COLUMN tenants.id IS '自增主键。';
COMMENT ON COLUMN tenants.tenant_id IS '租户唯一标识（业务主键）。';
COMMENT ON COLUMN tenants.name IS '租户名称。';
COMMENT ON COLUMN tenants.description IS '租户描述。';
COMMENT ON COLUMN tenants.tier IS '租户层级（用于策略分层）。';
COMMENT ON COLUMN tenants.quota_config IS '租户配额配置（可选）。';
COMMENT ON COLUMN tenants.rate_limit IS '租户级限流覆盖配置（可选）。';
COMMENT ON COLUMN tenants.allowed_services IS '允许使用的服务列表（可选）。';
COMMENT ON COLUMN tenants.status IS '租户状态（active/disabled 等）。';
COMMENT ON COLUMN tenants.metadata IS '扩展元数据。';
COMMENT ON COLUMN tenants.created_at IS '创建时间。';
COMMENT ON COLUMN tenants.updated_at IS '更新时间（由触发器自动维护）。';

-- 预置常用租户（可按需调整）
INSERT INTO tenants (tenant_id, name, description, tier, status, metadata)
VALUES
    ('default', 'Default', '系统默认租户', 'normal', 'active', '{}'::jsonb),
    ('public', 'Public', '匿名/公共租户', 'normal', 'active', '{}'::jsonb),
    ('local', 'Local', '本地开发租户', 'normal', 'active', '{}'::jsonb)
ON CONFLICT (tenant_id) DO NOTHING;

-- ============================================================
-- 5. 会话表
-- ============================================================
CREATE TABLE IF NOT EXISTS sessions (
    session_id VARCHAR(255) PRIMARY KEY,
    service_id VARCHAR(255),
    user_id VARCHAR(255),
    tenant_id VARCHAR(255),
    state JSONB NOT NULL DEFAULT '{}'::jsonb,
    history JSONB NOT NULL DEFAULT '[]'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE sessions IS '会话表：用于保存 session 状态、历史消息与扩展元数据。';
COMMENT ON COLUMN sessions.session_id IS '会话 ID（通常为 UUID 字符串）。';
COMMENT ON COLUMN sessions.service_id IS '关联服务 ID（可选；会话可能独立于服务创建）。';
COMMENT ON COLUMN sessions.user_id IS '用户 ID（可选）。';
COMMENT ON COLUMN sessions.tenant_id IS '租户 ID（可选）。';
COMMENT ON COLUMN sessions.state IS '会话状态（中间变量、上下文等）。';
COMMENT ON COLUMN sessions.history IS '历史消息列表（JSON 数组）。';
COMMENT ON COLUMN sessions.metadata IS '扩展元数据。';
COMMENT ON COLUMN sessions.config IS '会话级配置（如参数覆盖等）。';
COMMENT ON COLUMN sessions.status IS '会话状态（active/expired/closed 等）。';
COMMENT ON COLUMN sessions.expires_at IS '过期时间（可选）。';
COMMENT ON COLUMN sessions.created_at IS '创建时间。';
COMMENT ON COLUMN sessions.updated_at IS '更新时间（由触发器自动维护）。';

-- ============================================================
-- 6. 异步任务表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS tasks (
    task_id VARCHAR(255) PRIMARY KEY,
    request_id VARCHAR(255) NOT NULL,
    service_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255),
    tenant_id VARCHAR(255),
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    progress NUMERIC(5,2) NOT NULL DEFAULT 0 CHECK (progress >= 0 AND progress <= 100),
    request_data JSONB,
    result JSONB,
    error TEXT,
    callback_url TEXT,
    callback_sent BOOLEAN NOT NULL DEFAULT FALSE,
    priority INTEGER NOT NULL DEFAULT 0,
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    max_retries INTEGER NOT NULL DEFAULT 3 CHECK (max_retries >= 0),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

COMMENT ON TABLE tasks IS '异步任务表（预留）：用于持久化任务执行状态、回调与结果。';
COMMENT ON COLUMN tasks.task_id IS '任务 ID（通常为 UUID 字符串）。';
COMMENT ON COLUMN tasks.request_id IS '请求 ID（来自统一请求模型）。';
COMMENT ON COLUMN tasks.service_id IS '服务 ID。';
COMMENT ON COLUMN tasks.user_id IS '用户 ID（可选）。';
COMMENT ON COLUMN tasks.tenant_id IS '租户 ID（可选）。';
COMMENT ON COLUMN tasks.status IS '任务状态（pending/processing/completed/failed/cancelled）。';
COMMENT ON COLUMN tasks.progress IS '任务进度（0-100）。';
COMMENT ON COLUMN tasks.request_data IS '请求数据快照（JSON）。';
COMMENT ON COLUMN tasks.result IS '结果数据（JSON）。';
COMMENT ON COLUMN tasks.error IS '错误信息。';
COMMENT ON COLUMN tasks.callback_url IS '回调地址（可选）。';
COMMENT ON COLUMN tasks.callback_sent IS '是否已发送回调。';
COMMENT ON COLUMN tasks.priority IS '任务优先级（数值越大优先级越高/或按业务约定）。';
COMMENT ON COLUMN tasks.retry_count IS '已重试次数。';
COMMENT ON COLUMN tasks.max_retries IS '最大重试次数。';
COMMENT ON COLUMN tasks.metadata IS '扩展元数据。';
COMMENT ON COLUMN tasks.created_at IS '创建时间。';
COMMENT ON COLUMN tasks.updated_at IS '更新时间（由触发器自动维护）。';
COMMENT ON COLUMN tasks.started_at IS '开始执行时间。';
COMMENT ON COLUMN tasks.completed_at IS '完成时间。';

-- ============================================================
-- 6b. Knowledge Base (KBMS)
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

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

-- Image-Chunk Association Table (014_multimodal_chunks, 015_segment_images_fk)
-- Links image segments to text segments for multimodal retrieval
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

CREATE INDEX IF NOT EXISTS idx_segment_images_segment ON segment_images(segment_id);
CREATE INDEX IF NOT EXISTS idx_segment_images_image ON segment_images(image_segment_id);
CREATE INDEX IF NOT EXISTS idx_segments_content_type ON segments(content_type);
CREATE INDEX IF NOT EXISTS idx_segments_has_images ON segments(has_images) WHERE has_images = TRUE;
CREATE INDEX IF NOT EXISTS idx_segments_text_search ON segments USING GIN (text_search);
CREATE INDEX IF NOT EXISTS idx_segments_source_type ON segments(source_type);

-- Trigger: auto-populate text_search tsvector on INSERT/UPDATE
CREATE OR REPLACE FUNCTION segments_text_search_update() RETURNS trigger AS $$
BEGIN
    NEW.text_search := to_tsvector('simple', COALESCE(NEW.text, ''));
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_segments_text_search ON segments;
CREATE TRIGGER trg_segments_text_search
    BEFORE INSERT OR UPDATE OF text ON segments
    FOR EACH ROW
    EXECUTE FUNCTION segments_text_search_update();
CREATE INDEX IF NOT EXISTS idx_segments_language ON segments(language);

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

-- ============================================================
-- 7. 鉴权配置表
-- ============================================================
CREATE TABLE IF NOT EXISTS auth_config (
    id SERIAL PRIMARY KEY,
    config_type VARCHAR(50) NOT NULL UNIQUE,
    config JSONB NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE auth_config IS '鉴权配置表：持久化 JWT/API Key 等鉴权开关与配置（可选）。';
COMMENT ON COLUMN auth_config.id IS '自增主键。';
COMMENT ON COLUMN auth_config.config_type IS '配置类型（如 jwt / api_key / rbac 等）。';
COMMENT ON COLUMN auth_config.config IS '配置内容（JSON）。';
COMMENT ON COLUMN auth_config.enabled IS '是否启用该配置项。';
COMMENT ON COLUMN auth_config.created_at IS '创建时间。';
COMMENT ON COLUMN auth_config.updated_at IS '更新时间（由触发器自动维护）。';

-- ============================================================
-- 8. RBAC 角色权限表（预置）
-- ============================================================
CREATE TABLE IF NOT EXISTS rbac_roles (
    id SERIAL PRIMARY KEY,
    role_name VARCHAR(100) NOT NULL UNIQUE,
    description TEXT,
    permissions VARCHAR(100)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(100)[],
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE rbac_roles IS 'RBAC 角色表：定义角色与权限列表（当前系统也支持从配置文件加载）。';
COMMENT ON COLUMN rbac_roles.id IS '自增主键。';
COMMENT ON COLUMN rbac_roles.role_name IS '角色名（唯一）。';
COMMENT ON COLUMN rbac_roles.description IS '角色描述。';
COMMENT ON COLUMN rbac_roles.permissions IS '权限列表（如 service:invoke / admin:*）。';
COMMENT ON COLUMN rbac_roles.is_system IS '是否系统内置角色。';
COMMENT ON COLUMN rbac_roles.created_at IS '创建时间。';
COMMENT ON COLUMN rbac_roles.updated_at IS '更新时间（由触发器自动维护）。';

-- 插入默认角色
INSERT INTO rbac_roles (role_name, permissions, is_system) VALUES
    ('guest', ARRAY['service:invoke:public']::VARCHAR(100)[], TRUE),
    ('user', ARRAY['service:invoke', 'task:read']::VARCHAR(100)[], TRUE),
    ('developer', ARRAY['service:invoke', 'task:read', 'task:write', 'service:manage']::VARCHAR(100)[], TRUE),
    ('admin', ARRAY['admin:*']::VARCHAR(100)[], TRUE),
    ('platform_admin', ARRAY['admin:*']::VARCHAR(100)[], TRUE)
ON CONFLICT (role_name) DO NOTHING;

-- ============================================================
-- 9. 限流规则表
-- ============================================================
-- 兼容迁移：避免使用保留关键字 window 作为列名
DO $$
BEGIN
    IF to_regclass('public.rate_limit_config') IS NOT NULL
       AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'rate_limit_config'
              AND column_name = 'window'
        )
    THEN
        EXECUTE 'ALTER TABLE rate_limit_config RENAME COLUMN "window" TO window_seconds';
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS rate_limit_config (
    id SERIAL PRIMARY KEY,
    scope VARCHAR(50) NOT NULL,
    scope_id VARCHAR(255) NOT NULL DEFAULT '',
    requests INTEGER NOT NULL CHECK (requests > 0),
    window_seconds INTEGER NOT NULL CHECK (window_seconds > 0),
    burst INTEGER NOT NULL DEFAULT 0 CHECK (burst >= 0),
    strategy VARCHAR(50) NOT NULL DEFAULT 'sliding_window',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(scope, scope_id)
);

COMMENT ON TABLE rate_limit_config IS '限流规则表：支持 global/ip/user/tenant/service 等多范围规则。';
COMMENT ON COLUMN rate_limit_config.id IS '自增主键。';
COMMENT ON COLUMN rate_limit_config.scope IS '限流范围（global/ip/user/tenant/service）。';
COMMENT ON COLUMN rate_limit_config.scope_id IS '范围 ID（如 service_id、tenant_id；global 可为空字符串）。';
COMMENT ON COLUMN rate_limit_config.requests IS '允许的请求数。';
COMMENT ON COLUMN rate_limit_config.window_seconds IS '时间窗口（秒）。';
COMMENT ON COLUMN rate_limit_config.burst IS '突发容量（可选）。';
COMMENT ON COLUMN rate_limit_config.strategy IS '限流算法（sliding_window/fixed_window/token_bucket）。';
COMMENT ON COLUMN rate_limit_config.enabled IS '是否启用。';
COMMENT ON COLUMN rate_limit_config.priority IS '规则优先级（用于冲突时择优）。';
COMMENT ON COLUMN rate_limit_config.created_at IS '创建时间。';
COMMENT ON COLUMN rate_limit_config.updated_at IS '更新时间（由触发器自动维护）。';

-- 插入默认限流配置
INSERT INTO rate_limit_config (scope, scope_id, requests, window_seconds, burst, priority) VALUES
    ('global', '', 1000, 60, 100, 0)
ON CONFLICT (scope, scope_id) DO NOTHING;

-- ============================================================
-- 10. 语义缓存表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS semantic_cache (
    id SERIAL PRIMARY KEY,
    service_id VARCHAR(255) NOT NULL,
    input_hash VARCHAR(64) NOT NULL,
    input_embedding BYTEA,
    input_text TEXT,
    output_text TEXT NOT NULL,
    output_data JSONB,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    hit_count INTEGER NOT NULL DEFAULT 0 CHECK (hit_count >= 0),
    last_hit_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(service_id, input_hash)
);

COMMENT ON TABLE semantic_cache IS '语义缓存表（预留）：用于基于输入特征的结果缓存与命中统计。';
COMMENT ON COLUMN semantic_cache.id IS '自增主键。';
COMMENT ON COLUMN semantic_cache.service_id IS '服务 ID。';
COMMENT ON COLUMN semantic_cache.input_hash IS '输入哈希（用于快速匹配）。';
COMMENT ON COLUMN semantic_cache.input_embedding IS '输入向量（可选）。';
COMMENT ON COLUMN semantic_cache.input_text IS '输入文本（可选，便于排查）。';
COMMENT ON COLUMN semantic_cache.output_text IS '输出文本（兼容字段）。';
COMMENT ON COLUMN semantic_cache.output_data IS '输出结构化数据（JSON，可选）。';
COMMENT ON COLUMN semantic_cache.metadata IS '扩展元数据。';
COMMENT ON COLUMN semantic_cache.hit_count IS '命中次数。';
COMMENT ON COLUMN semantic_cache.last_hit_at IS '最后命中时间。';
COMMENT ON COLUMN semantic_cache.expires_at IS '过期时间（可选）。';
COMMENT ON COLUMN semantic_cache.created_at IS '创建时间。';

-- ============================================================
-- 11. 审计日志表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS audit_logs (
    id BIGSERIAL PRIMARY KEY,
    event_type VARCHAR(100) NOT NULL,
    user_id VARCHAR(255),
    tenant_id VARCHAR(255),
    ip_address VARCHAR(45),
    user_agent TEXT,
    resource_type VARCHAR(100),
    resource_id VARCHAR(255),
    action VARCHAR(50) NOT NULL,
    request_summary JSONB,
    response_summary JSONB,
    status VARCHAR(50) NOT NULL,
    error_message TEXT,
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE audit_logs IS '审计日志表（预留）：记录关键操作与请求/响应摘要，便于合规与排障。';
COMMENT ON COLUMN audit_logs.id IS '自增主键。';
COMMENT ON COLUMN audit_logs.event_type IS '事件类型（如 invoke/service/register 等）。';
COMMENT ON COLUMN audit_logs.user_id IS '用户 ID（可选）。';
COMMENT ON COLUMN audit_logs.tenant_id IS '租户 ID（可选）。';
COMMENT ON COLUMN audit_logs.ip_address IS '客户端 IP（IPv4/IPv6 字符串）。';
COMMENT ON COLUMN audit_logs.user_agent IS 'User-Agent。';
COMMENT ON COLUMN audit_logs.resource_type IS '资源类型（如 service/session/task）。';
COMMENT ON COLUMN audit_logs.resource_id IS '资源 ID（如 service_id）。';
COMMENT ON COLUMN audit_logs.action IS '动作（create/update/delete/invoke 等）。';
COMMENT ON COLUMN audit_logs.request_summary IS '请求摘要（JSON）。';
COMMENT ON COLUMN audit_logs.response_summary IS '响应摘要（JSON）。';
COMMENT ON COLUMN audit_logs.status IS '处理状态（success/failed 等）。';
COMMENT ON COLUMN audit_logs.error_message IS '错误信息（可选）。';
COMMENT ON COLUMN audit_logs.duration_ms IS '耗时（毫秒，可选）。';
COMMENT ON COLUMN audit_logs.created_at IS '创建时间。';

-- ============================================================
-- 12. 服务健康记录表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS service_health_records (
    id BIGSERIAL PRIMARY KEY,
    service_id VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL,
    response_time_ms INTEGER CHECK (response_time_ms IS NULL OR response_time_ms >= 0),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_message TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE service_health_records IS '服务健康记录表（预留）：记录健康检查结果与响应时间。';
COMMENT ON COLUMN service_health_records.id IS '自增主键。';
COMMENT ON COLUMN service_health_records.service_id IS '服务 ID。';
COMMENT ON COLUMN service_health_records.status IS '健康状态（healthy/unhealthy/timeout 等）。';
COMMENT ON COLUMN service_health_records.response_time_ms IS '响应时间（毫秒，可选）。';
COMMENT ON COLUMN service_health_records.details IS '检查详情（JSON）。';
COMMENT ON COLUMN service_health_records.error_message IS '错误信息（可选）。';
COMMENT ON COLUMN service_health_records.checked_at IS '检查时间。';

-- ============================================================
-- 13. 使用统计表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS usage_statistics (
    id BIGSERIAL PRIMARY KEY,
    dimension VARCHAR(50) NOT NULL,
    dimension_id VARCHAR(255) NOT NULL,
    period_type VARCHAR(20) NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0 CHECK (request_count >= 0),
    success_count BIGINT NOT NULL DEFAULT 0 CHECK (success_count >= 0),
    error_count BIGINT NOT NULL DEFAULT 0 CHECK (error_count >= 0),
    input_tokens BIGINT NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens BIGINT NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    avg_response_time_ms INTEGER CHECK (avg_response_time_ms IS NULL OR avg_response_time_ms >= 0),
    max_response_time_ms INTEGER CHECK (max_response_time_ms IS NULL OR max_response_time_ms >= 0),
    min_response_time_ms INTEGER CHECK (min_response_time_ms IS NULL OR min_response_time_ms >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(dimension, dimension_id, period_type, period_start)
);

COMMENT ON TABLE usage_statistics IS '使用统计表（预留）：按维度聚合请求量、token 量与延迟等指标。';
COMMENT ON COLUMN usage_statistics.id IS '自增主键。';
COMMENT ON COLUMN usage_statistics.dimension IS '统计维度（global/tenant/user/service 等）。';
COMMENT ON COLUMN usage_statistics.dimension_id IS '维度 ID（如 tenant_id、service_id）。';
COMMENT ON COLUMN usage_statistics.period_type IS '周期类型（minute/hour/day/month）。';
COMMENT ON COLUMN usage_statistics.period_start IS '周期起始时间。';
COMMENT ON COLUMN usage_statistics.request_count IS '请求总数。';
COMMENT ON COLUMN usage_statistics.success_count IS '成功请求数。';
COMMENT ON COLUMN usage_statistics.error_count IS '错误请求数。';
COMMENT ON COLUMN usage_statistics.input_tokens IS '输入 token 数。';
COMMENT ON COLUMN usage_statistics.output_tokens IS '输出 token 数。';
COMMENT ON COLUMN usage_statistics.avg_response_time_ms IS '平均响应时间（毫秒，可选）。';
COMMENT ON COLUMN usage_statistics.max_response_time_ms IS '最大响应时间（毫秒，可选）。';
COMMENT ON COLUMN usage_statistics.min_response_time_ms IS '最小响应时间（毫秒，可选）。';
COMMENT ON COLUMN usage_statistics.created_at IS '创建时间。';
COMMENT ON COLUMN usage_statistics.updated_at IS '更新时间（由触发器自动维护）。';

-- ============================================================
-- 14. LangGraph Thread 映射表（预留）
-- ============================================================
CREATE TABLE IF NOT EXISTS langgraph_threads (
    id SERIAL PRIMARY KEY,
    thread_id VARCHAR(255) NOT NULL UNIQUE,
    user_id VARCHAR(255) NOT NULL,
    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
    assistant_id VARCHAR(255),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    is_anonymous BOOLEAN NOT NULL DEFAULT FALSE,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_accessed_at TIMESTAMPTZ
);

COMMENT ON TABLE langgraph_threads IS 'LangGraph Thread 映射表（预留）：用于把网关用户/会话映射到 LangGraph thread。';
COMMENT ON COLUMN langgraph_threads.id IS '自增主键。';
COMMENT ON COLUMN langgraph_threads.thread_id IS 'LangGraph Thread ID。';
COMMENT ON COLUMN langgraph_threads.user_id IS '用户 ID。';
COMMENT ON COLUMN langgraph_threads.tenant_id IS '租户 ID。';
COMMENT ON COLUMN langgraph_threads.assistant_id IS 'Assistant/Graph ID（可选）。';
COMMENT ON COLUMN langgraph_threads.metadata IS '扩展元数据（可包含 session_id、标签等）。';
COMMENT ON COLUMN langgraph_threads.status IS '状态（active/expired/disabled 等）。';
COMMENT ON COLUMN langgraph_threads.is_anonymous IS '是否匿名 Thread。';
COMMENT ON COLUMN langgraph_threads.expires_at IS '过期时间（可选）。';
COMMENT ON COLUMN langgraph_threads.created_at IS '创建时间。';
COMMENT ON COLUMN langgraph_threads.updated_at IS '更新时间（由触发器自动维护）。';
COMMENT ON COLUMN langgraph_threads.last_accessed_at IS '最后访问时间。';

-- ============================================================
-- 15. 使用记录表（细粒度，保留30天）
-- Source: 017_usage_metrics
-- ============================================================
CREATE TABLE IF NOT EXISTS usage_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    request_id VARCHAR(64),
    service_id VARCHAR(64),
    assistant_id VARCHAR(64),
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(64),
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER GENERATED ALWAYS AS (input_tokens + output_tokens) STORED,
    input_cost_cents INTEGER NOT NULL DEFAULT 0,
    output_cost_cents INTEGER NOT NULL DEFAULT 0,
    total_cost_cents INTEGER GENERATED ALWAYS AS (input_cost_cents + output_cost_cents) STORED,
    latency_ms INTEGER,
    first_token_ms INTEGER,
    request_total_duration_ms INTEGER DEFAULT 0,
    llm_inference_duration_ms INTEGER DEFAULT 0,
    retrieval_duration_ms INTEGER DEFAULT 0,
    tool_call_duration_ms INTEGER DEFAULT 0,
    agent_or_graph_overhead_ms INTEGER DEFAULT 0,
    tool_call_breakdown JSONB DEFAULT '{}'::jsonb,
    error_type VARCHAR(32),
    trace_sampled BOOLEAN NOT NULL DEFAULT FALSE,
    sample_reason VARCHAR(32),
    status VARCHAR(32) DEFAULT 'success',
    request_type VARCHAR(32),
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW()
);

COMMENT ON TABLE usage_records IS '使用记录表：细粒度请求记录（保留30天）';

-- 请求追踪采样表（持久化可回放 Trace）
CREATE TABLE IF NOT EXISTS request_traces (
    trace_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id VARCHAR(64) NOT NULL,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    service_id VARCHAR(64) NOT NULL,
    assistant_id VARCHAR(64) NOT NULL DEFAULT '',
    provider VARCHAR(64) NOT NULL,
    model VARCHAR(128) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'success',
    error_type VARCHAR(32),
    request_total_duration_ms INTEGER NOT NULL DEFAULT 0,
    first_token_latency_ms INTEGER NOT NULL DEFAULT 0,
    llm_inference_duration_ms INTEGER NOT NULL DEFAULT 0,
    retrieval_duration_ms INTEGER NOT NULL DEFAULT 0,
    tool_call_duration_ms INTEGER NOT NULL DEFAULT 0,
    agent_or_graph_overhead_ms INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_cost_cents INTEGER NOT NULL DEFAULT 0,
    sample_reason VARCHAR(32) NOT NULL DEFAULT 'baseline',
    trace_steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE request_traces IS '请求追踪采样表：用于 Dashboard Trace 追踪与故障定位。';

-- ============================================================
-- 16. 每日聚合表（永久保留）
-- Source: 017_usage_metrics + 027_fix_usage_daily_aggregates_constraint
-- ============================================================
CREATE TABLE IF NOT EXISTS usage_daily_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL DEFAULT '',
    model VARCHAR(128) NOT NULL DEFAULT '',
    assistant_id VARCHAR(64) NOT NULL DEFAULT '',
    service_id VARCHAR(64) NOT NULL DEFAULT '',
    date DATE NOT NULL,
    request_count INTEGER DEFAULT 0,
    success_count INTEGER DEFAULT 0,
    error_count INTEGER DEFAULT 0,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0,
    avg_latency_ms INTEGER DEFAULT 0,
    p50_latency_ms INTEGER DEFAULT 0,
    p95_latency_ms INTEGER DEFAULT 0,
    p99_latency_ms INTEGER DEFAULT 0,
    avg_first_token_ms INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT uq_usage_daily_aggregates_dimensions
        UNIQUE (tenant_id, user_id, model, assistant_id, service_id, date)
);

COMMENT ON TABLE usage_daily_aggregates IS '每日聚合表：按维度聚合的每日使用统计（永久保留）';

-- ============================================================
-- 17. 每小时聚合表（保留7天）
-- Source: 017_usage_metrics + 027_fix_usage_daily_aggregates_constraint
-- ============================================================
CREATE TABLE IF NOT EXISTS usage_hourly_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL DEFAULT '',
    model VARCHAR(128) NOT NULL DEFAULT '',
    assistant_id VARCHAR(64) NOT NULL DEFAULT '',
    service_id VARCHAR(64) NOT NULL DEFAULT '',
    bucket_start TIMESTAMPTZ NOT NULL,
    date DATE NOT NULL,
    request_count INTEGER DEFAULT 0,
    success_count INTEGER DEFAULT 0,
    error_count INTEGER DEFAULT 0,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_cents BIGINT DEFAULT 0,
    avg_latency_ms INTEGER DEFAULT 0,
    avg_first_token_ms INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT uq_usage_hourly_aggregates_dimensions
        UNIQUE (tenant_id, user_id, model, assistant_id, service_id, bucket_start)
);

COMMENT ON TABLE usage_hourly_aggregates IS '每小时聚合表：按维度聚合的小时级使用统计（保留7天）';

-- ============================================================
-- 18. 用户配额表
-- Source: 017_usage_metrics
-- ============================================================
CREATE TABLE IF NOT EXISTS user_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    daily_token_limit BIGINT,
    monthly_token_limit BIGINT,
    monthly_cost_limit_cents BIGINT,
    requests_per_minute INTEGER,
    requests_per_day INTEGER,
    current_daily_tokens BIGINT DEFAULT 0,
    current_monthly_tokens BIGINT DEFAULT 0,
    current_monthly_cost_cents BIGINT DEFAULT 0,
    current_daily_requests INTEGER DEFAULT 0,
    daily_reset_at TIMESTAMPTZ,
    monthly_reset_at TIMESTAMPTZ,
    is_blocked BOOLEAN DEFAULT FALSE,
    blocked_reason VARCHAR(256),
    blocked_at TIMESTAMPTZ,
    warning_threshold INTEGER DEFAULT 80,
    overage_strategy VARCHAR(32) NOT NULL DEFAULT 'allow_but_alert'
        CHECK (overage_strategy IN ('hard_block', 'rate_limit', 'downgrade_model', 'allow_but_alert')),
    downgraded_model VARCHAR(128),
    temporary_extra_tokens BIGINT NOT NULL DEFAULT 0,
    temporary_extra_cost_cents BIGINT NOT NULL DEFAULT 0,
    temporary_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (tenant_id, user_id)
);

COMMENT ON TABLE user_quotas IS '用户配额表：配额限制与当前用量追踪';

-- ============================================================
-- 19. 模型定价表
-- Source: 017_usage_metrics
-- ============================================================
CREATE TABLE IF NOT EXISTS model_pricing (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    model VARCHAR(128) NOT NULL,
    provider VARCHAR(64),
    display_name VARCHAR(256),
    input_price_per_1k DECIMAL(10, 6) NOT NULL,
    output_price_per_1k DECIMAL(10, 6) NOT NULL,
    context_window INTEGER,
    max_output_tokens INTEGER,
    supports_vision BOOLEAN DEFAULT FALSE,
    supports_tools BOOLEAN DEFAULT TRUE,
    supports_streaming BOOLEAN DEFAULT TRUE,
    is_active BOOLEAN DEFAULT TRUE,
    effective_from TIMESTAMPTZ DEFAULT NOW(),
    effective_to TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (model)
);

COMMENT ON TABLE model_pricing IS '模型定价表：按模型的 token 定价配置';

-- ============================================================
-- 20. 配额告警记录表
-- Source: 017_usage_metrics
-- ============================================================
CREATE TABLE IF NOT EXISTS quota_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    alert_type VARCHAR(64) NOT NULL,
    threshold_value BIGINT,
    current_value BIGINT,
    limit_value BIGINT,
    message TEXT,
    is_acknowledged BOOLEAN DEFAULT FALSE,
    acknowledged_at TIMESTAMPTZ,
    acknowledged_by VARCHAR(64),
    created_at TIMESTAMPTZ DEFAULT NOW()
);

COMMENT ON TABLE quota_alerts IS '配额告警记录表：配额超限或接近限额的告警';

-- ============================================================
-- 21. 安全事件每日聚合表
-- Source: 018_security_events + 030_fix_timestamp_and_security_constraint
-- ============================================================
CREATE TABLE IF NOT EXISTS security_event_daily_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL DEFAULT '',
    service_id VARCHAR(64) NOT NULL DEFAULT '',
    event_type VARCHAR(32) NOT NULL,
    date DATE NOT NULL,
    event_count INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT uq_security_event_daily_dimensions
        UNIQUE (tenant_id, user_id, service_id, event_type, date)
);

COMMENT ON TABLE security_event_daily_aggregates IS '安全事件每日聚合表：认证失败、限流事件统计';

-- ============================================================
-- 22. Session Memory 表（会话级记忆）
-- Source: 024_assistant_memory + 026_memory_tenant_isolation
-- ============================================================
CREATE TABLE IF NOT EXISTS session_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(100) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    key VARCHAR(255) NOT NULL,
    value JSONB NOT NULL,
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT session_memory_tenant_session_key_unique
        UNIQUE (tenant_id, session_id, key)
);

COMMENT ON TABLE session_memory IS '会话记忆表：跨重连持久化的会话级键值存储';

-- ============================================================
-- 23. User Memory 表（用户级长期记忆）
-- Source: 024_assistant_memory + 026_memory_tenant_isolation
-- ============================================================
CREATE TABLE IF NOT EXISTS user_memory (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(100) NOT NULL,
    user_id VARCHAR(100) NOT NULL,
    key VARCHAR(255) NOT NULL,
    value JSONB NOT NULL,
    metadata JSONB,
    access_count INTEGER DEFAULT 0,
    last_accessed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT user_memory_tenant_user_key_unique
        UNIQUE (tenant_id, user_id, key)
);

COMMENT ON TABLE user_memory IS '用户记忆表：跨会话持久化的偏好、模式与知识';

-- ============================================================
-- 24. KBMS 增强表
-- Source: 002_kbms_enhancements
-- ============================================================

-- 子块表：用于 parent-child 检索
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

COMMENT ON TABLE child_chunks IS '子块表：parent-child 检索策略中的 child chunk';

-- 数据集关键词表
CREATE TABLE IF NOT EXISTS dataset_keyword_tables (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL UNIQUE REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    keyword_table TEXT NOT NULL DEFAULT '{}',
    data_source_type VARCHAR(50) NOT NULL DEFAULT 'database',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_keyword_tables IS '数据集关键词表：BM25 等关键词检索索引';

-- 数据集处理规则表
CREATE TABLE IF NOT EXISTS dataset_process_rules (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
    rules JSONB NOT NULL DEFAULT '{}',
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE dataset_process_rules IS '数据集处理规则：分块策略与预处理配置';

-- 数据集查询记录表
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

COMMENT ON TABLE dataset_queries IS '数据集查询记录：检索请求日志';

-- ============================================================
-- 25. 代理路由与计费
-- Source: 003_proxy_enhancements
-- ============================================================

-- 代理路由表
CREATE TABLE IF NOT EXISTS proxy_routes (
    id SERIAL PRIMARY KEY,
    route_name VARCHAR(255) NOT NULL UNIQUE,
    route_pattern VARCHAR(500) NOT NULL,
    service_id VARCHAR(255) REFERENCES services(service_id) ON DELETE CASCADE,
    priority INTEGER NOT NULL DEFAULT 0,
    methods VARCHAR(20)[] NOT NULL DEFAULT ARRAY['GET','POST','PUT','PATCH','DELETE']::VARCHAR(20)[],
    upstream_url VARCHAR(500),
    path_rewrite VARCHAR(255),
    strip_prefix BOOLEAN NOT NULL DEFAULT TRUE,
    auth_required BOOLEAN NOT NULL DEFAULT TRUE,
    rate_limit_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    rate_limit_requests INTEGER DEFAULT 100,
    rate_limit_window INTEGER DEFAULT 60,
    timeout_seconds INTEGER DEFAULT 60,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    metadata JSONB NOT NULL DEFAULT '{}',
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE proxy_routes IS '代理路由表：URL 路由到上游服务的映射';

-- 计费事件表
CREATE TABLE IF NOT EXISTS billing_events (
    id BIGSERIAL PRIMARY KEY,
    event_id VARCHAR(255) NOT NULL UNIQUE,
    request_id VARCHAR(255) NOT NULL,
    service_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255),
    tenant_id VARCHAR(255),
    input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    total_tokens INTEGER NOT NULL DEFAULT 0 CHECK (total_tokens >= 0),
    model VARCHAR(255),
    assistant_id VARCHAR(255),
    duration_ms NUMERIC(12,2) CHECK (duration_ms IS NULL OR duration_ms >= 0),
    status VARCHAR(50) NOT NULL DEFAULT 'completed',
    cost_amount NUMERIC(12,6),
    cost_currency VARCHAR(10) DEFAULT 'USD',
    metadata JSONB NOT NULL DEFAULT '{}',
    event_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE billing_events IS '计费事件表：每次请求的 token 用量与费用记录';

-- ============================================================
-- 26. Confluence 集成
-- Source: 004_confluence_integration + 007/008/010/011/012/013/016
-- ============================================================

-- Confluence 连接表
CREATE TABLE IF NOT EXISTS confluence_connections (
    connection_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
    name VARCHAR(255) NOT NULL,
    domain VARCHAR(255) NOT NULL,
    email VARCHAR(255) NOT NULL,
    api_token TEXT NOT NULL,
    base_url VARCHAR(512),
    sync_mode VARCHAR(50) NOT NULL DEFAULT 'manual',
    polling_interval_minutes INTEGER DEFAULT 60,
    status VARCHAR(50) NOT NULL DEFAULT 'active',
    last_sync_at TIMESTAMPTZ,
    last_error TEXT,
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    owner_id VARCHAR(255)
);

COMMENT ON TABLE confluence_connections IS 'Confluence 连接配置：Atlassian 实例连接信息';

-- Confluence Space 绑定表
CREATE TABLE IF NOT EXISTS confluence_space_bindings (
    binding_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    connection_id VARCHAR(255) NOT NULL REFERENCES confluence_connections(connection_id) ON DELETE CASCADE,
    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    space_key VARCHAR(255) NOT NULL,
    space_id VARCHAR(255),
    space_name VARCHAR(255),
    include_patterns JSONB DEFAULT '[]',
    exclude_patterns JSONB DEFAULT '[]',
    max_depth INTEGER DEFAULT 10,
    include_attachments BOOLEAN DEFAULT FALSE,
    include_comments BOOLEAN DEFAULT FALSE,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    last_sync_at TIMESTAMPTZ,
    synced_page_count INTEGER DEFAULT 0,
    total_page_count INTEGER DEFAULT 0,
    last_error TEXT,
    created_by VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    tenant_id VARCHAR(255),
    root_page_id VARCHAR(255),
    root_page_title VARCHAR(512),
    sync_images BOOLEAN DEFAULT FALSE,
    image_max_size_bytes INTEGER DEFAULT 3145728,
    last_sync_type VARCHAR(50),
    owner_id VARCHAR(255),
    sync_mode VARCHAR(50) NOT NULL DEFAULT 'manual'
        CHECK (sync_mode IN ('manual', 'polling')),
    polling_interval_minutes INTEGER DEFAULT 60
        CHECK (polling_interval_minutes >= 5 AND polling_interval_minutes <= 1440),
    last_incremental_sync_at TIMESTAMPTZ,
    next_sync_at TIMESTAMPTZ,
    sync_enabled BOOLEAN DEFAULT TRUE,
    root_page_ids TEXT[] DEFAULT '{}',
    root_page_titles TEXT[] DEFAULT '{}',
    UNIQUE (connection_id, space_key, dataset_id)
);

COMMENT ON TABLE confluence_space_bindings IS 'Confluence Space 绑定：Space 与 Dataset 的映射关系';

-- Confluence 页面跟踪表
CREATE TABLE IF NOT EXISTS confluence_pages (
    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    binding_id VARCHAR(255) NOT NULL REFERENCES confluence_space_bindings(binding_id) ON DELETE CASCADE,
    document_id VARCHAR(255) REFERENCES documents(document_id) ON DELETE SET NULL,
    page_id VARCHAR(255) NOT NULL,
    space_key VARCHAR(255) NOT NULL,
    title VARCHAR(512) NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    content_hash VARCHAR(64),
    parent_page_id VARCHAR(255),
    depth INTEGER DEFAULT 0,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    last_synced_at TIMESTAMPTZ,
    confluence_updated_at TIMESTAMPTZ,
    error TEXT,
    labels JSONB DEFAULT '[]',
    web_url TEXT,
    author VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Image sync fields (010_image_segments)
    image_count INTEGER DEFAULT 0,
    images_synced_at TIMESTAMPTZ,
    -- Page-level sync fields (013_page_level_sync)
    sync_mode VARCHAR(50),
    polling_interval_minutes INTEGER,
    sync_enabled BOOLEAN DEFAULT TRUE,
    next_sync_at TIMESTAMPTZ,
    sync_priority INTEGER DEFAULT 0,
    UNIQUE (binding_id, page_id)
);

COMMENT ON TABLE confluence_pages IS 'Confluence 页面跟踪：同步状态与版本管理';

-- Confluence 同步任务表
CREATE TABLE IF NOT EXISTS confluence_sync_tasks (
    task_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    binding_id VARCHAR(255) REFERENCES confluence_space_bindings(binding_id) ON DELETE CASCADE,
    page_id VARCHAR(255),
    task_type VARCHAR(50) NOT NULL,
    priority INTEGER DEFAULT 0,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    retry_count INTEGER DEFAULT 0,
    max_retries INTEGER DEFAULT 3,
    progress NUMERIC(5,2) DEFAULT 0,
    total_items INTEGER DEFAULT 0,
    processed_items INTEGER DEFAULT 0,
    error TEXT,
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    owner_id VARCHAR(255)
);

COMMENT ON TABLE confluence_sync_tasks IS 'Confluence 同步任务：批量/增量同步任务追踪';

-- Confluence Webhook 表
CREATE TABLE IF NOT EXISTS confluence_webhooks (
    webhook_id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
    connection_id VARCHAR(255) NOT NULL REFERENCES confluence_connections(connection_id) ON DELETE CASCADE,
    confluence_webhook_id VARCHAR(255),
    webhook_secret VARCHAR(255) NOT NULL,
    callback_url TEXT NOT NULL,
    events JSONB NOT NULL DEFAULT '["page_created", "page_updated", "page_removed"]',
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    last_event_at TIMESTAMPTZ,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE confluence_webhooks IS 'Confluence Webhook：实时页面变更事件监听';

-- Confluence 图片同步表 (010_image_segments)
CREATE TABLE IF NOT EXISTS confluence_image_sync (
    id BIGSERIAL PRIMARY KEY,
    page_record_id VARCHAR(255) REFERENCES confluence_pages(id) ON DELETE CASCADE,
    attachment_id VARCHAR(100) NOT NULL,
    filename VARCHAR(255) NOT NULL,
    media_type VARCHAR(100),
    file_size INTEGER,
    storage_url TEXT,
    segment_id VARCHAR(255),
    status VARCHAR(50) DEFAULT 'pending',
    error_message TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    synced_at TIMESTAMPTZ,
    UNIQUE (page_record_id, attachment_id)
);

COMMENT ON TABLE confluence_image_sync IS 'Confluence 图片同步：页面内嵌图片的提取与存储';

-- ============================================================
-- 27. 账号权限系统
-- Source: 005_account_permission_system + 006_user_extra_permissions
-- ============================================================

-- 权限定义表
CREATE TABLE IF NOT EXISTS permissions (
    id SERIAL PRIMARY KEY,
    permission_code VARCHAR(100) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    category VARCHAR(50) NOT NULL,
    resource VARCHAR(100) NOT NULL,
    action VARCHAR(50) NOT NULL,
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE permissions IS '权限定义表：细粒度权限编码与分类';

-- 角色-权限关联表
CREATE TABLE IF NOT EXISTS role_permissions (
    id SERIAL PRIMARY KEY,
    role_name VARCHAR(100) NOT NULL,
    permission_code VARCHAR(100) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (role_name, permission_code)
);

COMMENT ON TABLE role_permissions IS '角色-权限关联表：角色与权限的多对多映射';

-- 用户-角色关联表
CREATE TABLE IF NOT EXISTS user_roles (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    role_name VARCHAR(100) NOT NULL,
    granted_by VARCHAR(255),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    UNIQUE (user_id, role_name)
);

COMMENT ON TABLE user_roles IS '用户-角色关联表：用户与角色的多对多映射';

-- Only the bootstrap operator created by the system is a platform admin.
-- Tenant administrators retain the tenant-scoped `admin` role.
UPDATE users
SET roles = CASE
        WHEN 'platform_admin' = ANY(roles) THEN roles
        ELSE array_append(roles, 'platform_admin'::VARCHAR(50))
    END,
    updated_at = NOW()
WHERE user_id = 'admin'
  AND created_by = 'system';

INSERT INTO user_roles (user_id, role_name, granted_by)
SELECT user_id, 'platform_admin', 'system'
FROM users
WHERE user_id = 'admin'
  AND created_by = 'system'
ON CONFLICT (user_id, role_name) DO NOTHING;

-- 用户-权限直接授权表
CREATE TABLE IF NOT EXISTS user_permissions (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    permission_code VARCHAR(100) NOT NULL,
    granted_by VARCHAR(255),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    note TEXT,
    UNIQUE (user_id, permission_code)
);

COMMENT ON TABLE user_permissions IS '用户直接权限表：绕过角色的权限直授';

-- 邮箱域名配置表
CREATE TABLE IF NOT EXISTS email_domain_config (
    id SERIAL PRIMARY KEY,
    domain VARCHAR(255) NOT NULL UNIQUE,
    is_allowed BOOLEAN NOT NULL DEFAULT TRUE,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE email_domain_config IS '邮箱域名配置：允许/拒绝的注册域名白名单';

-- 登录审计表
CREATE TABLE IF NOT EXISTS login_audit (
    id BIGSERIAL PRIMARY KEY,
    user_id VARCHAR(255),
    email VARCHAR(255),
    action VARCHAR(50) NOT NULL,
    ip_address VARCHAR(45),
    user_agent TEXT,
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE login_audit IS '登录审计表：登录/登出/密码修改等安全事件';

-- 密码历史表
CREATE TABLE IF NOT EXISTS password_history (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE password_history IS '密码历史表：防止密码重复使用';

-- ============================================================
-- 28. Artifacts（会话制品存储）
-- Source: 021_artifacts
-- ============================================================
CREATE TABLE IF NOT EXISTS artifacts (
    artifact_id VARCHAR(64) PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,
    message_id VARCHAR(64),
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    type VARCHAR(32) NOT NULL,
    format VARCHAR(32) NOT NULL,
    title VARCHAR(255) NOT NULL,
    filename VARCHAR(255) NOT NULL,
    storage_key VARCHAR(512) NOT NULL,
    size_bytes BIGINT NOT NULL DEFAULT 0,
    mime_type VARCHAR(128),
    source VARCHAR(32) NOT NULL DEFAULT 'ai',
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE artifacts IS 'Artifacts 表：AI 生成的代码、文档等制品存储引用';

-- ============================================================
-- 29. LLM 服务商与模型配置
-- Source: 022_llm_providers_models
-- ============================================================

-- LLM 服务商表
CREATE TABLE IF NOT EXISTS llm_providers (
    tenant_id VARCHAR(100) NOT NULL,
    provider_id VARCHAR(50) NOT NULL,
    display_name VARCHAR(100) NOT NULL,
    api_type VARCHAR(20) DEFAULT 'openai',
    base_url VARCHAR(500),
    api_key_encrypted TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    is_enabled BOOLEAN DEFAULT TRUE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (tenant_id, provider_id)
);

COMMENT ON TABLE llm_providers IS 'LLM 服务商表：多租户 LLM 提供商配置';

-- LLM 模型表
CREATE TABLE IF NOT EXISTS llm_models (
    tenant_id VARCHAR(100) NOT NULL,
    model_id VARCHAR(100) NOT NULL,
    provider_id VARCHAR(50) NOT NULL,
    display_name VARCHAR(100) NOT NULL,
    context_window INTEGER DEFAULT 128000,
    max_output_tokens INTEGER DEFAULT 4096,
    supports_vision BOOLEAN DEFAULT FALSE,
    supports_tools BOOLEAN DEFAULT TRUE,
    input_price_per_1k DECIMAL(10,6) DEFAULT 0,
    output_price_per_1k DECIMAL(10,6) DEFAULT 0,
    access_level VARCHAR(20) DEFAULT 'public',
    is_enabled BOOLEAN DEFAULT TRUE,
    sort_order INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    catalog_capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
    capability_overrides JSONB NOT NULL DEFAULT '{}'::jsonb,
    capability_revision BIGINT NOT NULL DEFAULT 1,
    CONSTRAINT llm_models_catalog_capabilities_object
        CHECK (jsonb_typeof(catalog_capabilities) = 'object'),
    CONSTRAINT llm_models_capability_overrides_object
        CHECK (jsonb_typeof(capability_overrides) = 'object'),
    CONSTRAINT llm_models_capability_revision_positive
        CHECK (capability_revision > 0),
    PRIMARY KEY (tenant_id, model_id)
);

COMMENT ON TABLE llm_models IS 'LLM 模型表：多租户模型配置与定价';
COMMENT ON COLUMN llm_models.catalog_capabilities IS
    'Versioned provider catalog capability profile; provider sync may update it.';
COMMENT ON COLUMN llm_models.capability_overrides IS
    'Tenant administrator capability overrides; provider sync must preserve it.';
COMMENT ON COLUMN llm_models.capability_revision IS
    'Optimistic concurrency revision for effective capability changes.';

-- ============================================================
-- 30. 文档版本控制
-- Source: 025_document_version_control
-- ============================================================

-- 文档版本表
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

COMMENT ON TABLE document_versions IS '文档版本表：文档内容变更历史';

-- 版本保留策略表
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

COMMENT ON TABLE version_retention_policies IS '版本保留策略：自动清理旧版本的配置';

-- ============================================================
-- 旧数据清理函数
-- Source: 017_usage_metrics
-- ============================================================
CREATE OR REPLACE FUNCTION cleanup_old_usage_records(retention_days INTEGER DEFAULT 30)
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM usage_records
    WHERE created_at < CURRENT_TIMESTAMP - (retention_days || ' days')::INTERVAL;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============================================================
-- 索引
-- ============================================================
CREATE INDEX IF NOT EXISTS idx_services_service_type ON services(service_type);
CREATE INDEX IF NOT EXISTS idx_services_status ON services(status);
CREATE INDEX IF NOT EXISTS idx_services_tags ON services USING GIN(tags);

CREATE INDEX IF NOT EXISTS idx_api_keys_tenant_id ON api_keys(tenant_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys(user_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_enabled ON api_keys(enabled);
CREATE INDEX IF NOT EXISTS idx_api_keys_expires_at ON api_keys(expires_at);
CREATE INDEX IF NOT EXISTS idx_api_keys_allowed_models_gin ON api_keys USING GIN(allowed_models);

CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON users(tenant_id);
CREATE INDEX IF NOT EXISTS idx_users_tier ON users(tier);
CREATE INDEX IF NOT EXISTS idx_users_status ON users(status);

CREATE INDEX IF NOT EXISTS idx_sessions_service_id ON sessions(service_id);
CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_tenant_id ON sessions(tenant_id);
CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);

CREATE INDEX IF NOT EXISTS idx_tasks_request_id ON tasks(request_id);
CREATE INDEX IF NOT EXISTS idx_tasks_service_id ON tasks(service_id);
CREATE INDEX IF NOT EXISTS idx_tasks_user_id ON tasks(user_id);
CREATE INDEX IF NOT EXISTS idx_tasks_tenant_id ON tasks(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_datasets_tenant_id ON datasets(tenant_id);
CREATE INDEX IF NOT EXISTS idx_datasets_visibility ON datasets(visibility);
CREATE INDEX IF NOT EXISTS idx_datasets_active_created_at ON datasets(created_at DESC) WHERE is_deleted = FALSE;
CREATE INDEX IF NOT EXISTS idx_datasets_active_tenant_visibility ON datasets(tenant_id, visibility, created_at DESC) WHERE is_deleted = FALSE;

CREATE INDEX IF NOT EXISTS idx_documents_dataset_id ON documents(dataset_id);
CREATE INDEX IF NOT EXISTS idx_documents_status ON documents(status);
CREATE INDEX IF NOT EXISTS idx_documents_created_at ON documents(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_segments_dataset_id ON segments(dataset_id);
CREATE INDEX IF NOT EXISTS idx_segments_document_id ON segments(document_id);
CREATE INDEX IF NOT EXISTS idx_segments_vector_id ON segments(vector_id);
CREATE INDEX IF NOT EXISTS idx_segments_content_hash ON segments(document_id, content_hash);

CREATE INDEX IF NOT EXISTS idx_dataset_permissions_dataset_id ON dataset_permissions(dataset_id);
CREATE INDEX IF NOT EXISTS idx_dataset_permissions_subject ON dataset_permissions(subject_type, subject_id);

CREATE INDEX IF NOT EXISTS idx_semantic_cache_service_id ON semantic_cache(service_id);
CREATE INDEX IF NOT EXISTS idx_semantic_cache_expires_at ON semantic_cache(expires_at);

CREATE INDEX IF NOT EXISTS idx_audit_logs_event_type ON audit_logs(event_type);
CREATE INDEX IF NOT EXISTS idx_audit_logs_user_id ON audit_logs(user_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_tenant_id ON audit_logs(tenant_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_created_at ON audit_logs(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_health_records_service_id ON service_health_records(service_id);
CREATE INDEX IF NOT EXISTS idx_health_records_checked_at ON service_health_records(checked_at DESC);

CREATE INDEX IF NOT EXISTS idx_usage_statistics_period_start ON usage_statistics(period_start DESC);

CREATE INDEX IF NOT EXISTS idx_langgraph_threads_user_id ON langgraph_threads(user_id);
CREATE INDEX IF NOT EXISTS idx_langgraph_threads_tenant_id ON langgraph_threads(tenant_id);
CREATE INDEX IF NOT EXISTS idx_langgraph_threads_status ON langgraph_threads(status);

-- api_keys additional indexes (from 003_proxy_enhancements)
CREATE INDEX IF NOT EXISTS idx_api_keys_key_id ON api_keys(key_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_key_prefix ON api_keys(key_prefix);

-- users additional indexes (from 005_account_permission_system)
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_unique ON users(LOWER(email)) WHERE email IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_username ON users(username) WHERE username IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_status_tenant ON users(status, tenant_id);

-- datasets additional indexes (from 002_kbms_enhancements)
CREATE INDEX IF NOT EXISTS idx_datasets_kb_type ON datasets(kb_type);
CREATE INDEX IF NOT EXISTS idx_datasets_tenant_kb_type ON datasets(tenant_id, kb_type);

-- documents additional indexes (from 002/004 migrations)
CREATE INDEX IF NOT EXISTS idx_documents_enabled ON documents(enabled);
CREATE INDEX IF NOT EXISTS idx_documents_archived ON documents(archived);
CREATE INDEX IF NOT EXISTS idx_documents_batch ON documents(batch);
CREATE INDEX IF NOT EXISTS idx_documents_confluence_page ON documents(confluence_page_id) WHERE confluence_page_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_documents_confluence_binding ON documents(confluence_binding_id) WHERE confluence_binding_id IS NOT NULL;

-- segments additional indexes (from 002 migrations)
CREATE INDEX IF NOT EXISTS idx_segments_enabled ON segments(enabled);
CREATE INDEX IF NOT EXISTS idx_segments_status ON segments(status);
CREATE INDEX IF NOT EXISTS idx_segments_index_node_id ON segments(index_node_id);
CREATE INDEX IF NOT EXISTS idx_segments_image_attachment ON segments(image_attachment_id) WHERE image_attachment_id IS NOT NULL;

-- segment_images additional indexes
CREATE INDEX IF NOT EXISTS idx_segment_images_proximity ON segment_images(segment_id, proximity_score DESC);

-- usage_records indexes
CREATE INDEX IF NOT EXISTS idx_usage_tenant_date ON usage_records (tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_user_date ON usage_records (user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_model ON usage_records (model, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage_records (provider, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_assistant ON usage_records (assistant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_service ON usage_records (service_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_status_error_type ON usage_records (status, error_type, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_created_at ON usage_records (created_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_records_request_identity
ON usage_records (
    tenant_id,
    COALESCE(request_id, ''),
    COALESCE(service_id, ''),
    COALESCE(request_type, '')
)
WHERE request_id IS NOT NULL AND request_id <> '';

-- request_traces indexes
CREATE INDEX IF NOT EXISTS idx_request_traces_tenant_created ON request_traces (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_request_id ON request_traces (request_id);
CREATE INDEX IF NOT EXISTS idx_request_traces_service ON request_traces (service_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_user ON request_traces (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_provider_model ON request_traces (provider, model, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_status_error ON request_traces (status, error_type, created_at DESC);

-- usage_daily_aggregates indexes
CREATE INDEX IF NOT EXISTS idx_usage_daily_tenant_date ON usage_daily_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_daily_user_date ON usage_daily_aggregates (user_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_daily_model_date ON usage_daily_aggregates (model, date);

-- usage_hourly_aggregates indexes
CREATE INDEX IF NOT EXISTS idx_usage_hourly_tenant_date ON usage_hourly_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_usage_hourly_bucket ON usage_hourly_aggregates (bucket_start);

-- user_quotas indexes
CREATE INDEX IF NOT EXISTS idx_quota_tenant ON user_quotas (tenant_id);
CREATE INDEX IF NOT EXISTS idx_quota_blocked ON user_quotas (is_blocked) WHERE is_blocked = TRUE;

-- model_pricing indexes
CREATE INDEX IF NOT EXISTS idx_pricing_provider ON model_pricing (provider);
CREATE INDEX IF NOT EXISTS idx_pricing_active ON model_pricing (is_active) WHERE is_active = TRUE;

-- quota_alerts indexes
CREATE INDEX IF NOT EXISTS idx_alerts_tenant_user ON quota_alerts (tenant_id, user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_alerts_unacked ON quota_alerts (is_acknowledged, created_at) WHERE is_acknowledged = FALSE;

-- security_event_daily_aggregates indexes
CREATE INDEX IF NOT EXISTS idx_security_event_daily_tenant_date ON security_event_daily_aggregates (tenant_id, date);
CREATE INDEX IF NOT EXISTS idx_security_event_daily_user_date ON security_event_daily_aggregates (user_id, date);
CREATE INDEX IF NOT EXISTS idx_security_event_daily_service_date ON security_event_daily_aggregates (service_id, date);

-- session_memory indexes
CREATE INDEX IF NOT EXISTS idx_session_memory_tenant_session_key ON session_memory(tenant_id, session_id, key);
CREATE INDEX IF NOT EXISTS idx_session_memory_tenant_session ON session_memory(tenant_id, session_id);

-- user_memory indexes
CREATE INDEX IF NOT EXISTS idx_user_memory_tenant_user_key ON user_memory(tenant_id, user_id, key);
CREATE INDEX IF NOT EXISTS idx_user_memory_tenant_user ON user_memory(tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_user_memory_access_count ON user_memory(access_count DESC) WHERE access_count > 0;

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

-- proxy_routes indexes
CREATE INDEX IF NOT EXISTS idx_proxy_routes_service_id ON proxy_routes(service_id);
CREATE INDEX IF NOT EXISTS idx_proxy_routes_priority ON proxy_routes(priority DESC);
CREATE INDEX IF NOT EXISTS idx_proxy_routes_enabled ON proxy_routes(enabled);

-- billing_events indexes
CREATE INDEX IF NOT EXISTS idx_billing_events_request_id ON billing_events(request_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_service_id ON billing_events(service_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_user_id ON billing_events(user_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_tenant_id ON billing_events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_event_time ON billing_events(event_time DESC);
CREATE INDEX IF NOT EXISTS idx_billing_events_created_at ON billing_events(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_billing_events_time_tenant ON billing_events(event_time DESC, tenant_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_time_user ON billing_events(event_time DESC, user_id);

-- confluence_connections indexes
CREATE INDEX IF NOT EXISTS idx_confluence_connections_tenant ON confluence_connections(tenant_id);
CREATE INDEX IF NOT EXISTS idx_confluence_connections_domain ON confluence_connections(domain);
CREATE INDEX IF NOT EXISTS idx_confluence_connections_status ON confluence_connections(status);
CREATE INDEX IF NOT EXISTS idx_confluence_connections_owner ON confluence_connections(owner_id);

-- confluence_space_bindings indexes
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_connection ON confluence_space_bindings(connection_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_dataset ON confluence_space_bindings(dataset_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_space ON confluence_space_bindings(space_key);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_status ON confluence_space_bindings(status);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_tenant ON confluence_space_bindings(tenant_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_owner ON confluence_space_bindings(owner_id);
CREATE INDEX IF NOT EXISTS idx_confluence_bindings_next_sync ON confluence_space_bindings(next_sync_at)
    WHERE sync_enabled = TRUE AND sync_mode = 'polling';

-- confluence_pages indexes
CREATE INDEX IF NOT EXISTS idx_confluence_pages_binding ON confluence_pages(binding_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_document ON confluence_pages(document_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_page_id ON confluence_pages(page_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_parent ON confluence_pages(parent_page_id);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_status ON confluence_pages(status);
CREATE INDEX IF NOT EXISTS idx_confluence_pages_next_sync ON confluence_pages(next_sync_at)
    WHERE sync_enabled = TRUE AND sync_mode IS NOT NULL;

-- confluence_sync_tasks indexes
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_binding ON confluence_sync_tasks(binding_id);
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_status ON confluence_sync_tasks(status, priority DESC);
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_created ON confluence_sync_tasks(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_confluence_sync_tasks_owner ON confluence_sync_tasks(owner_id);

-- confluence_webhooks indexes
CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_connection ON confluence_webhooks(connection_id);
CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_status ON confluence_webhooks(status);
CREATE INDEX IF NOT EXISTS idx_confluence_webhooks_secret ON confluence_webhooks(webhook_secret);

-- confluence_image_sync indexes
CREATE INDEX IF NOT EXISTS idx_confluence_image_sync_page ON confluence_image_sync(page_record_id);
CREATE INDEX IF NOT EXISTS idx_confluence_image_sync_status ON confluence_image_sync(status);

-- permissions indexes
CREATE INDEX IF NOT EXISTS idx_permissions_category ON permissions(category);
CREATE INDEX IF NOT EXISTS idx_permissions_resource ON permissions(resource);

-- role_permissions indexes
CREATE INDEX IF NOT EXISTS idx_role_permissions_role ON role_permissions(role_name);
CREATE INDEX IF NOT EXISTS idx_role_permissions_perm ON role_permissions(permission_code);

-- user_roles indexes
CREATE INDEX IF NOT EXISTS idx_user_roles_user_id ON user_roles(user_id);
CREATE INDEX IF NOT EXISTS idx_user_roles_role_name ON user_roles(role_name);

-- user_permissions indexes
CREATE INDEX IF NOT EXISTS idx_user_permissions_user_id ON user_permissions(user_id);
CREATE INDEX IF NOT EXISTS idx_user_permissions_permission ON user_permissions(permission_code);

-- login_audit indexes
CREATE INDEX IF NOT EXISTS idx_login_audit_user_id ON login_audit(user_id);
CREATE INDEX IF NOT EXISTS idx_login_audit_email ON login_audit(email);
CREATE INDEX IF NOT EXISTS idx_login_audit_action ON login_audit(action);
CREATE INDEX IF NOT EXISTS idx_login_audit_created_at ON login_audit(created_at DESC);

-- password_history indexes
CREATE INDEX IF NOT EXISTS idx_password_history_user_id ON password_history(user_id);

-- artifacts indexes
CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_tenant_user ON artifacts(tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_type ON artifacts(type);
CREATE INDEX IF NOT EXISTS idx_artifacts_created ON artifacts(created_at DESC);

-- llm_providers indexes
CREATE INDEX IF NOT EXISTS idx_llm_providers_tenant ON llm_providers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_llm_providers_enabled ON llm_providers(tenant_id, is_enabled);

-- llm_models indexes
CREATE INDEX IF NOT EXISTS idx_llm_models_tenant ON llm_models(tenant_id);
CREATE INDEX IF NOT EXISTS idx_llm_models_provider ON llm_models(tenant_id, provider_id);
CREATE INDEX IF NOT EXISTS idx_llm_models_enabled ON llm_models(tenant_id, is_enabled);

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
-- 触发器：自动更新 updated_at
-- ============================================================
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_services_updated_at ON services;
CREATE TRIGGER update_services_updated_at BEFORE UPDATE ON services
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_api_keys_updated_at ON api_keys;
CREATE TRIGGER update_api_keys_updated_at BEFORE UPDATE ON api_keys
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_users_updated_at ON users;
CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_tenants_updated_at ON tenants;
CREATE TRIGGER update_tenants_updated_at BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_sessions_updated_at ON sessions;
CREATE TRIGGER update_sessions_updated_at BEFORE UPDATE ON sessions
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_tasks_updated_at ON tasks;
CREATE TRIGGER update_tasks_updated_at BEFORE UPDATE ON tasks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

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

DROP TRIGGER IF EXISTS update_auth_config_updated_at ON auth_config;
CREATE TRIGGER update_auth_config_updated_at BEFORE UPDATE ON auth_config
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_rbac_roles_updated_at ON rbac_roles;
CREATE TRIGGER update_rbac_roles_updated_at BEFORE UPDATE ON rbac_roles
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_rate_limit_config_updated_at ON rate_limit_config;
CREATE TRIGGER update_rate_limit_config_updated_at BEFORE UPDATE ON rate_limit_config
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_usage_statistics_updated_at ON usage_statistics;
CREATE TRIGGER update_usage_statistics_updated_at BEFORE UPDATE ON usage_statistics
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_langgraph_threads_updated_at ON langgraph_threads;
CREATE TRIGGER update_langgraph_threads_updated_at BEFORE UPDATE ON langgraph_threads
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_usage_daily_aggregates_updated_at ON usage_daily_aggregates;
CREATE TRIGGER update_usage_daily_aggregates_updated_at BEFORE UPDATE ON usage_daily_aggregates
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_user_quotas_updated_at ON user_quotas;
CREATE TRIGGER update_user_quotas_updated_at BEFORE UPDATE ON user_quotas
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_model_pricing_updated_at ON model_pricing;
CREATE TRIGGER update_model_pricing_updated_at BEFORE UPDATE ON model_pricing
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_security_event_daily_aggregates_updated_at ON security_event_daily_aggregates;
CREATE TRIGGER update_security_event_daily_aggregates_updated_at BEFORE UPDATE ON security_event_daily_aggregates
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_session_memory_timestamp ON session_memory;
CREATE TRIGGER update_session_memory_timestamp BEFORE UPDATE ON session_memory
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_user_memory_timestamp ON user_memory;
CREATE TRIGGER update_user_memory_timestamp BEFORE UPDATE ON user_memory
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_child_chunks_updated_at ON child_chunks;
CREATE TRIGGER update_child_chunks_updated_at BEFORE UPDATE ON child_chunks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_dataset_keyword_tables_updated_at ON dataset_keyword_tables;
CREATE TRIGGER update_dataset_keyword_tables_updated_at BEFORE UPDATE ON dataset_keyword_tables
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_dataset_process_rules_updated_at ON dataset_process_rules;
CREATE TRIGGER update_dataset_process_rules_updated_at BEFORE UPDATE ON dataset_process_rules
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_proxy_routes_updated_at ON proxy_routes;
CREATE TRIGGER update_proxy_routes_updated_at BEFORE UPDATE ON proxy_routes
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_connections_updated_at ON confluence_connections;
CREATE TRIGGER update_confluence_connections_updated_at BEFORE UPDATE ON confluence_connections
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_space_bindings_updated_at ON confluence_space_bindings;
CREATE TRIGGER update_confluence_space_bindings_updated_at BEFORE UPDATE ON confluence_space_bindings
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_pages_updated_at ON confluence_pages;
CREATE TRIGGER update_confluence_pages_updated_at BEFORE UPDATE ON confluence_pages
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_sync_tasks_updated_at ON confluence_sync_tasks;
CREATE TRIGGER update_confluence_sync_tasks_updated_at BEFORE UPDATE ON confluence_sync_tasks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_confluence_webhooks_updated_at ON confluence_webhooks;
CREATE TRIGGER update_confluence_webhooks_updated_at BEFORE UPDATE ON confluence_webhooks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_permissions_updated_at ON permissions;
CREATE TRIGGER update_permissions_updated_at BEFORE UPDATE ON permissions
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_email_domain_config_updated_at ON email_domain_config;
CREATE TRIGGER update_email_domain_config_updated_at BEFORE UPDATE ON email_domain_config
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS update_version_retention_policies_updated_at ON version_retention_policies;
CREATE TRIGGER update_version_retention_policies_updated_at BEFORE UPDATE ON version_retention_policies
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- Image count maintenance trigger (from 014_multimodal_chunks)
-- Auto-updates has_images and image_count on segments
-- ============================================================
CREATE OR REPLACE FUNCTION update_segment_image_counts()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE segments
        SET has_images = TRUE,
            image_count = (SELECT COUNT(*) FROM segment_images WHERE segment_id = NEW.segment_id)
        WHERE segment_id = NEW.segment_id;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE segments
        SET image_count = (SELECT COUNT(*) FROM segment_images WHERE segment_id = OLD.segment_id),
            has_images = (SELECT COUNT(*) > 0 FROM segment_images WHERE segment_id = OLD.segment_id)
        WHERE segment_id = OLD.segment_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_update_segment_image_counts ON segment_images;
CREATE TRIGGER trg_update_segment_image_counts
AFTER INSERT OR DELETE ON segment_images
FOR EACH ROW
EXECUTE FUNCTION update_segment_image_counts();
