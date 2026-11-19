-- API Keys 表增强
-- 添加 key_id 和 key_prefix 字段用于显示和管理

-- 添加新字段（如果不存在）
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS key_id VARCHAR(32) UNIQUE;
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS key_prefix VARCHAR(12);
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS scopes TEXT[] DEFAULT ARRAY['knowledge:read', 'knowledge:write'];

-- 为现有记录生成 key_id (如果为空)
UPDATE api_keys
SET key_id = 'ak_' || substring(key_hash from 1 for 24)
WHERE key_id IS NULL;

-- 为现有记录生成 key_prefix (如果为空)
UPDATE api_keys
SET key_prefix = 'agk_' || substring(key_hash from 1 for 8)
WHERE key_prefix IS NULL;

-- 索引
CREATE INDEX IF NOT EXISTS idx_api_keys_key_id ON api_keys(key_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_key_prefix ON api_keys(key_prefix);
