-- ============================================================
-- Migration: 003_proxy_enhancements
-- Description: 透明代理功能增强
-- 
-- 变更内容：
-- 1. services 表 connector_config JSONB 字段增加透明代理配置文档
-- 2. 新增 proxy_routes 表用于动态路由配置
-- 3. 新增 billing_events 表用于计费事件存储
-- ============================================================

-- ============================================================
-- 1. 为 services 表添加注释说明 connector_config 的透明代理配置
-- ============================================================
COMMENT ON COLUMN services.connector_config IS '连接器配置（JSON），支持以下字段：
- base_url: 基础 URL（通用）
- proxy_mode: 代理模式 (transparent | adapter)
- upstream_url: 上游服务 URL
- upstream_urls: 上游服务 URL 列表（多实例）
- assistant_id: LangGraph Assistant ID
- path_rewrite: 路径重写前缀
- strip_prefix: 是否去除代理前缀 (默认 true)
- timeout_connect: 连接超时（秒，默认 5）
- timeout_read: 读取超时（秒，默认 300）
- timeout_write: 写入超时（秒，默认 60）
- timeout_pool: 连接池超时（秒，默认 60）
- auth_token: 内部认证 token
- forward_auth: 是否转发原始 Authorization 头 (默认 true)
- load_balance_strategy: 负载均衡策略 (round_robin | least_connections | random)
- headers: 自定义请求头（字典）
';

-- ============================================================
-- 2. 新增 proxy_routes 表 - 动态路由配置
-- ============================================================
CREATE TABLE IF NOT EXISTS proxy_routes (
    id SERIAL PRIMARY KEY,
    route_name VARCHAR(255) NOT NULL UNIQUE,
    route_pattern VARCHAR(500) NOT NULL,          -- 路由匹配模式，如 /api/v1/proxy/{service}/*
    service_id VARCHAR(255) REFERENCES services(service_id) ON DELETE CASCADE,
    
    -- 路由配置
    priority INTEGER NOT NULL DEFAULT 0,          -- 优先级（数字越大优先级越高）
    methods VARCHAR(20)[] NOT NULL DEFAULT ARRAY['GET', 'POST', 'PUT', 'PATCH', 'DELETE']::VARCHAR(20)[],
    
    -- 上游配置（可覆盖 service 配置）
    upstream_url VARCHAR(500),                    -- 覆盖 service 的上游 URL
    path_rewrite VARCHAR(255),                    -- 路径重写规则
    strip_prefix BOOLEAN NOT NULL DEFAULT TRUE,
    
    -- 中间件配置
    auth_required BOOLEAN NOT NULL DEFAULT TRUE,  -- 是否需要鉴权
    rate_limit_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    rate_limit_requests INTEGER DEFAULT 100,
    rate_limit_window INTEGER DEFAULT 60,
    
    -- 超时配置
    timeout_seconds INTEGER DEFAULT 60,
    
    -- 状态
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    
    -- 元数据
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    description TEXT,
    
    -- 时间戳
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE proxy_routes IS '透明代理路由配置表：支持动态路由规则和中间件配置。';
COMMENT ON COLUMN proxy_routes.route_name IS '路由名称（唯一标识）';
COMMENT ON COLUMN proxy_routes.route_pattern IS '路由匹配模式，支持通配符 * 和参数 {param}';
COMMENT ON COLUMN proxy_routes.service_id IS '关联的服务 ID';
COMMENT ON COLUMN proxy_routes.priority IS '路由优先级（数字越大优先级越高）';
COMMENT ON COLUMN proxy_routes.methods IS '允许的 HTTP 方法';
COMMENT ON COLUMN proxy_routes.upstream_url IS '上游 URL（可覆盖 service 配置）';
COMMENT ON COLUMN proxy_routes.path_rewrite IS '路径重写规则';
COMMENT ON COLUMN proxy_routes.strip_prefix IS '是否去除代理前缀';
COMMENT ON COLUMN proxy_routes.auth_required IS '是否需要鉴权';
COMMENT ON COLUMN proxy_routes.rate_limit_enabled IS '是否启用限流';
COMMENT ON COLUMN proxy_routes.rate_limit_requests IS '限流请求数';
COMMENT ON COLUMN proxy_routes.rate_limit_window IS '限流时间窗口（秒）';
COMMENT ON COLUMN proxy_routes.timeout_seconds IS '请求超时（秒）';
COMMENT ON COLUMN proxy_routes.enabled IS '是否启用';
COMMENT ON COLUMN proxy_routes.metadata IS '扩展元数据';
COMMENT ON COLUMN proxy_routes.description IS '路由描述';

-- 索引
CREATE INDEX IF NOT EXISTS idx_proxy_routes_service_id ON proxy_routes(service_id);
CREATE INDEX IF NOT EXISTS idx_proxy_routes_enabled ON proxy_routes(enabled);
CREATE INDEX IF NOT EXISTS idx_proxy_routes_priority ON proxy_routes(priority DESC);

-- ============================================================
-- 3. 新增 billing_events 表 - 计费事件存储
-- ============================================================
CREATE TABLE IF NOT EXISTS billing_events (
    id BIGSERIAL PRIMARY KEY,
    event_id VARCHAR(255) NOT NULL UNIQUE,        -- 事件唯一 ID
    
    -- 关联信息
    request_id VARCHAR(255) NOT NULL,
    service_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255),
    tenant_id VARCHAR(255),
    
    -- Token 计数
    input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    total_tokens INTEGER NOT NULL DEFAULT 0 CHECK (total_tokens >= 0),
    
    -- 模型信息
    model VARCHAR(255),
    assistant_id VARCHAR(255),
    
    -- 请求信息
    duration_ms NUMERIC(12, 2) CHECK (duration_ms IS NULL OR duration_ms >= 0),
    status VARCHAR(50) NOT NULL DEFAULT 'completed',  -- completed | failed | partial
    
    -- 计费信息
    cost_amount NUMERIC(12, 6),                   -- 费用金额
    cost_currency VARCHAR(10) DEFAULT 'USD',      -- 货币
    
    -- 元数据
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    
    -- 时间戳
    event_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE billing_events IS '计费事件表：记录 LLM 调用的 token 使用量和计费信息。';
COMMENT ON COLUMN billing_events.event_id IS '事件唯一 ID';
COMMENT ON COLUMN billing_events.request_id IS '请求 ID';
COMMENT ON COLUMN billing_events.service_id IS '服务 ID';
COMMENT ON COLUMN billing_events.user_id IS '用户 ID';
COMMENT ON COLUMN billing_events.tenant_id IS '租户 ID';
COMMENT ON COLUMN billing_events.input_tokens IS '输入 token 数';
COMMENT ON COLUMN billing_events.output_tokens IS '输出 token 数';
COMMENT ON COLUMN billing_events.total_tokens IS '总 token 数';
COMMENT ON COLUMN billing_events.model IS '模型名称';
COMMENT ON COLUMN billing_events.assistant_id IS 'Assistant ID';
COMMENT ON COLUMN billing_events.duration_ms IS '请求耗时（毫秒）';
COMMENT ON COLUMN billing_events.status IS '状态（completed/failed/partial）';
COMMENT ON COLUMN billing_events.cost_amount IS '费用金额';
COMMENT ON COLUMN billing_events.cost_currency IS '货币类型';
COMMENT ON COLUMN billing_events.metadata IS '扩展元数据';
COMMENT ON COLUMN billing_events.event_time IS '事件发生时间';

-- 索引
CREATE INDEX IF NOT EXISTS idx_billing_events_request_id ON billing_events(request_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_service_id ON billing_events(service_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_user_id ON billing_events(user_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_tenant_id ON billing_events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_event_time ON billing_events(event_time DESC);
CREATE INDEX IF NOT EXISTS idx_billing_events_created_at ON billing_events(created_at DESC);

-- 按时间分区索引（用于快速查询近期数据）
CREATE INDEX IF NOT EXISTS idx_billing_events_time_user ON billing_events(event_time DESC, user_id);
CREATE INDEX IF NOT EXISTS idx_billing_events_time_tenant ON billing_events(event_time DESC, tenant_id);

-- ============================================================
-- 4. 触发器：自动更新 updated_at
-- ============================================================
DROP TRIGGER IF EXISTS update_proxy_routes_updated_at ON proxy_routes;
CREATE TRIGGER update_proxy_routes_updated_at BEFORE UPDATE ON proxy_routes
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- 5. 插入示例透明代理服务配置
-- ============================================================
INSERT INTO services (
    service_id,
    name,
    description,
    version,
    service_type,
    connector_type,
    connector_config,
    supported_modes,
    session_enabled,
    timeout,
    status,
    tags
) VALUES (
    'langgraph-proxy-example',
    'LangGraph Proxy Example',
    '透明代理模式的 LangGraph 服务示例配置',
    '1.0.0',
    'langgraph',
    'http',
    '{
        "proxy_mode": "transparent",
        "upstream_url": "http://localhost:8123",
        "assistant_id": "agent",
        "path_rewrite": null,
        "strip_prefix": true,
        "timeout_connect": 5,
        "timeout_read": 300,
        "timeout_write": 60,
        "auth_token": null,
        "forward_auth": true,
        "load_balance_strategy": "round_robin"
    }'::jsonb,
    ARRAY['sync', 'stream']::VARCHAR(50)[],
    TRUE,
    300,
    'inactive',  -- 默认未激活，需要手动启用
    ARRAY['example', 'langgraph', 'proxy']::VARCHAR(100)[]
) ON CONFLICT (service_id) DO NOTHING;

-- ============================================================
-- 6. 视图：活跃的透明代理服务
-- ============================================================
CREATE OR REPLACE VIEW v_active_proxy_services AS
SELECT 
    service_id,
    name,
    description,
    connector_config->>'upstream_url' AS upstream_url,
    connector_config->>'assistant_id' AS assistant_id,
    connector_config->>'load_balance_strategy' AS load_balance_strategy,
    (connector_config->>'timeout_read')::INTEGER AS timeout_read,
    status,
    created_at,
    updated_at
FROM services
WHERE status = 'active'
  AND connector_config->>'proxy_mode' = 'transparent';

COMMENT ON VIEW v_active_proxy_services IS '活跃的透明代理服务视图';

-- ============================================================
-- 7. 用户计费汇总视图
-- ============================================================
CREATE OR REPLACE VIEW v_user_billing_summary AS
SELECT 
    user_id,
    tenant_id,
    DATE_TRUNC('day', event_time) AS billing_date,
    COUNT(*) AS request_count,
    SUM(input_tokens) AS total_input_tokens,
    SUM(output_tokens) AS total_output_tokens,
    SUM(total_tokens) AS total_tokens,
    SUM(cost_amount) AS total_cost,
    AVG(duration_ms) AS avg_duration_ms
FROM billing_events
WHERE status = 'completed'
GROUP BY user_id, tenant_id, DATE_TRUNC('day', event_time);

COMMENT ON VIEW v_user_billing_summary IS '用户计费汇总视图（按天）';

