-- 046: User Connectors — OAuth-based third-party integrations
-- Supports: Confluence, Outlook, GitHub, Gmail, Google Drive, Slack, etc.

CREATE TABLE IF NOT EXISTS user_connectors (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       VARCHAR(64) NOT NULL,
    user_id         VARCHAR(64) NOT NULL,

    -- Connector identity
    provider        VARCHAR(64) NOT NULL,  -- 'confluence', 'outlook', 'github', 'gmail', 'google_drive', 'slack'
    display_name    VARCHAR(256),          -- User-visible name (e.g., "My Confluence")

    -- OAuth tokens (encrypted at rest via application-level encryption)
    access_token    TEXT,
    refresh_token   TEXT,
    token_expires_at TIMESTAMPTZ,
    token_scopes    TEXT,                  -- Comma-separated scopes granted

    -- Provider-specific metadata
    provider_metadata JSONB DEFAULT '{}',  -- e.g., {"cloud_id": "xxx", "site_url": "https://mycompany.atlassian.net"}

    -- State
    status          VARCHAR(32) DEFAULT 'connected',  -- 'connected', 'expired', 'revoked', 'error'
    last_used_at    TIMESTAMPTZ,
    last_error      TEXT,

    -- Audit
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    updated_at      TIMESTAMPTZ DEFAULT NOW(),

    -- Each user can have at most one connection per provider
    UNIQUE (tenant_id, user_id, provider)
);

CREATE INDEX idx_user_connectors_user ON user_connectors(tenant_id, user_id);
CREATE INDEX idx_user_connectors_provider ON user_connectors(provider, status);

-- Connector definitions (admin-configured, available to all users)
CREATE TABLE IF NOT EXISTS connector_configs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       VARCHAR(64) NOT NULL DEFAULT '',
    provider        VARCHAR(64) NOT NULL,  -- 'confluence', 'outlook', etc.
    display_name    VARCHAR(256) NOT NULL,
    description     TEXT,
    icon_url        VARCHAR(512),

    -- OAuth app credentials (encrypted)
    client_id       TEXT NOT NULL,
    client_secret   TEXT,

    -- OAuth endpoints (provider-specific)
    auth_url        TEXT NOT NULL,
    token_url       TEXT NOT NULL,
    scopes          TEXT NOT NULL,          -- Default scopes to request
    redirect_uri    TEXT,

    -- Feature flags
    enabled         BOOLEAN DEFAULT true,
    supports_sync   BOOLEAN DEFAULT false,  -- Can sync content to KB
    supports_search BOOLEAN DEFAULT true,   -- Can search on-demand

    -- Provider-specific config
    extra_config    JSONB DEFAULT '{}',     -- e.g., {"api_base": "https://api.atlassian.com"}

    created_at      TIMESTAMPTZ DEFAULT NOW(),
    updated_at      TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE (tenant_id, provider)
);

-- Insert default connector configurations
INSERT INTO connector_configs (tenant_id, provider, display_name, description, client_id, auth_url, token_url, scopes, extra_config) VALUES
('', 'confluence', 'Confluence', 'Atlassian Confluence — 搜索和引用企业知识库页面',
 '', 'https://auth.atlassian.com/authorize', 'https://auth.atlassian.com/oauth/token',
 'read:confluence-content.all search:confluence read:confluence-space.summary offline_access',
 '{"audience": "api.atlassian.com", "api_base": "https://api.atlassian.com"}'),

('', 'outlook', 'Outlook', 'Microsoft Outlook — 搜索和发送电子邮件',
 '', 'https://login.microsoftonline.com/common/oauth2/v2.0/authorize', 'https://login.microsoftonline.com/common/oauth2/v2.0/token',
 'offline_access Mail.Read Mail.Send Calendars.Read',
 '{"api_base": "https://graph.microsoft.com/v1.0"}'),

('', 'github', 'GitHub', 'GitHub — 搜索代码、管理Issues和Pull Requests',
 '', 'https://github.com/login/oauth/authorize', 'https://github.com/login/oauth/access_token',
 'repo read:user',
 '{"api_base": "https://api.github.com"}'),

('', 'gmail', 'Gmail', 'Gmail — 搜索和发送邮件',
 '', 'https://accounts.google.com/o/oauth2/v2/auth', 'https://oauth2.googleapis.com/token',
 'https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send',
 '{"api_base": "https://gmail.googleapis.com"}'),

('', 'google_drive', 'Google Drive', 'Google Drive — 搜索和读取文档',
 '', 'https://accounts.google.com/o/oauth2/v2/auth', 'https://oauth2.googleapis.com/token',
 'https://www.googleapis.com/auth/drive.readonly',
 '{"api_base": "https://www.googleapis.com/drive/v3"}'),

('', 'slack', 'Slack', 'Slack — 搜索消息和频道',
 '', 'https://slack.com/oauth/v2/authorize', 'https://slack.com/api/oauth.v2.access',
 'channels:read channels:history search:read',
 '{"api_base": "https://slack.com/api"}')

ON CONFLICT DO NOTHING;
