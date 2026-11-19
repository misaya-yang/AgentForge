-- ============================================================
-- Migration: 086_platform_admin_role
-- Description: Separate platform administration from tenant administration
-- Date: 2026-08-18
-- ============================================================

-- Global service configuration and metrics require both their normal
-- capability and an explicit platform-level identity.  Keep tenant `admin`
-- roles tenant-scoped; only the system-created bootstrap operator is promoted.
INSERT INTO rbac_roles (role_name, description, permissions, is_system)
VALUES (
    'platform_admin',
    'Platform administrator for global gateway resources',
    ARRAY['admin:*']::VARCHAR(100)[],
    TRUE
)
ON CONFLICT (role_name) DO UPDATE SET
    description = EXCLUDED.description,
    permissions = EXCLUDED.permissions,
    is_system = TRUE,
    updated_at = NOW();

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
