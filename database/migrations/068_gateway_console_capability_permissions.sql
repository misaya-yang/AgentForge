-- =============================================================================
-- Migration 068 — Seed Gateway console capability permissions
-- =============================================================================
-- Keeps the role API grantable after capability-gated management routes were
-- expanded beyond the original account-permission seed.
-- =============================================================================

INSERT INTO permissions (permission_code, name, description, category, resource, action, is_system)
VALUES
    ('console:metrics:view', 'View Gateway Metrics', 'Access gateway metrics and realtime dashboards', 'console', 'metrics', 'view', TRUE),
    ('console:usage:view', 'View Gateway Usage', 'Access usage analytics and billing summaries', 'console', 'usage', 'view', TRUE),
    ('console:quota:view', 'View Gateway Quotas', 'View quota configuration and consumption', 'console', 'quota', 'view', TRUE),
    ('console:quota:edit', 'Edit Gateway Quotas', 'Modify user and tenant quota configuration', 'console', 'quota', 'edit', TRUE),
    ('console:rate_limits:edit', 'Edit Gateway Rate Limits', 'Modify gateway rate limit policies', 'console', 'rate_limits', 'edit', TRUE),
    ('console:providers:view', 'View Model Providers', 'View provider configuration', 'console', 'providers', 'view', TRUE),
    ('console:providers:edit', 'Edit Model Providers', 'Create and update provider configuration', 'console', 'providers', 'edit', TRUE),
    ('console:skills:view', 'View Skills', 'View assistant skill registry entries', 'console', 'skills', 'view', TRUE),
    ('console:skills:edit', 'Edit Skills', 'Upload, refresh, and delete assistant skills', 'console', 'skills', 'edit', TRUE),
    ('console:mcp:view', 'View MCP Servers', 'View MCP servers and discovered tools', 'console', 'mcp', 'view', TRUE),
    ('console:mcp:edit', 'Edit MCP Servers', 'Refresh and manage MCP connections', 'console', 'mcp', 'edit', TRUE),
    ('console:models:edit', 'Edit Model Configuration', 'Create and update model configuration', 'console', 'models', 'edit', TRUE),
    ('console:eval:view', 'View Eval Console', 'Access agent trace evaluation console and trace queries', 'console', 'eval', 'view', TRUE),
    ('console:eval:run', 'Run Eval Operations', 'Score traces, run evaluators, and manage eval datasets and experiments', 'console', 'eval', 'run', TRUE)
ON CONFLICT (permission_code) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    category = EXCLUDED.category,
    resource = EXCLUDED.resource,
    action = EXCLUDED.action,
    is_system = EXCLUDED.is_system,
    updated_at = NOW();

-- Developer: full gateway console management.
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'developer', unnest(ARRAY[
    'console:metrics:view',
    'console:usage:view',
    'console:quota:view',
    'console:quota:edit',
    'console:rate_limits:edit',
    'console:providers:view',
    'console:providers:edit',
    'console:skills:view',
    'console:skills:edit',
    'console:mcp:view',
    'console:mcp:edit',
    'console:models:edit',
    'console:eval:view',
    'console:eval:run'
]::VARCHAR(100)[])
ON CONFLICT (role_name, permission_code) DO NOTHING;

UPDATE rbac_roles
SET
    permissions = (
        SELECT ARRAY(
            SELECT DISTINCT unnest(
                permissions || ARRAY[
                    'console:metrics:view',
                    'console:usage:view',
                    'console:quota:view',
                    'console:quota:edit',
                    'console:rate_limits:edit',
                    'console:providers:view',
                    'console:providers:edit',
                    'console:skills:view',
                    'console:skills:edit',
                    'console:mcp:view',
                    'console:mcp:edit',
                    'console:models:edit',
                    'console:eval:view',
                    'console:eval:run'
                ]::VARCHAR(100)[]
            )
        )
    ),
    updated_at = NOW()
WHERE role_name = 'developer';

-- Manager: operational read access only.
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'manager', unnest(ARRAY[
    'console:metrics:view',
    'console:usage:view',
    'console:quota:view',
    'console:providers:view',
    'console:skills:view',
    'console:mcp:view',
    'console:eval:view'
]::VARCHAR(100)[])
ON CONFLICT (role_name, permission_code) DO NOTHING;

UPDATE rbac_roles
SET
    permissions = (
        SELECT ARRAY(
            SELECT DISTINCT unnest(
                permissions || ARRAY[
                    'console:metrics:view',
                    'console:usage:view',
                    'console:quota:view',
                    'console:providers:view',
                    'console:skills:view',
                    'console:mcp:view',
                    'console:eval:view'
                ]::VARCHAR(100)[]
            )
        )
    ),
    updated_at = NOW()
WHERE role_name = 'manager';
