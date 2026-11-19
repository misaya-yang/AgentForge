-- ============================================================
-- Migration: 006_user_extra_permissions
-- Description: Add user-specific permissions (extra permissions beyond roles)
-- Date: 2026-01-05
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. User permissions table - Direct permission assignments to users
-- ============================================================
CREATE TABLE IF NOT EXISTS user_permissions (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    permission_code VARCHAR(100) NOT NULL,
    granted_by VARCHAR(255),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    note TEXT,
    UNIQUE(user_id, permission_code)
);

COMMENT ON TABLE user_permissions IS 'Direct permission assignments to users (extra permissions beyond roles)';
COMMENT ON COLUMN user_permissions.granted_by IS 'User who granted this permission';
COMMENT ON COLUMN user_permissions.expires_at IS 'Optional expiration time for temporary permissions';
COMMENT ON COLUMN user_permissions.note IS 'Optional note explaining why this permission was granted';

CREATE INDEX IF NOT EXISTS idx_user_permissions_user_id ON user_permissions(user_id);
CREATE INDEX IF NOT EXISTS idx_user_permissions_permission ON user_permissions(permission_code);

-- ============================================================
-- 2. Update rbac_roles table - Add is_system column if not exists
-- ============================================================
ALTER TABLE rbac_roles ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT FALSE;

-- ============================================================
-- 3. Update role descriptions
-- ============================================================
UPDATE rbac_roles SET
    description = 'System Administrator with full access to all features',
    is_system = TRUE
WHERE role_name = 'admin';

UPDATE rbac_roles SET
    description = 'Customer Service Staff',
    is_system = TRUE
WHERE role_name = 'cs_staff';

UPDATE rbac_roles SET
    description = 'Sales Staff',
    is_system = TRUE
WHERE role_name = 'sales_staff';

UPDATE rbac_roles SET
    description = 'Department Manager',
    is_system = TRUE
WHERE role_name = 'manager';

UPDATE rbac_roles SET
    description = 'Regular user with basic access',
    is_system = TRUE
WHERE role_name = 'user';

UPDATE rbac_roles SET
    description = 'Guest user with limited access',
    is_system = TRUE
WHERE role_name = 'guest';

UPDATE rbac_roles SET
    description = 'Developer with extended access',
    is_system = FALSE
WHERE role_name = 'developer';

-- ============================================================
-- 4. Add developer role permissions if not exists
-- ============================================================
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'developer', unnest(ARRAY[
    'console:dashboard:view',
    'console:settings:view',
    'console:services:view',
    'console:services:edit',
    'conversation:playground:access',
    'conversation:thread:create',
    'conversation:thread:delete',
    'knowledge:dataset:view',
    'knowledge:dataset:create',
    'knowledge:dataset:edit'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- Update developer role in rbac_roles
UPDATE rbac_roles SET
    permissions = ARRAY[
        'console:dashboard:view',
        'console:settings:view',
        'console:services:view',
        'console:services:edit',
        'conversation:playground:access',
        'conversation:thread:create',
        'conversation:thread:delete',
        'knowledge:dataset:view',
        'knowledge:dataset:create',
        'knowledge:dataset:edit'
    ]::VARCHAR(100)[]
WHERE role_name = 'developer';

-- ============================================================
-- Migration complete
-- ============================================================
