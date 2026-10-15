-- ============================================================
-- Migration: 005_account_permission_system
-- Description: Account Management and Permission System
-- Date: 2026-01-05
-- ============================================================

SET client_encoding TO 'UTF8';

-- ============================================================
-- 1. Enhanced users table - Add password and login tracking fields
-- ============================================================
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_hash VARCHAR(255);
ALTER TABLE users ADD COLUMN IF NOT EXISTS force_password_change BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_changed_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS login_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN IF NOT EXISTS locked_until TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_login_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_login_ip VARCHAR(45);
ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS created_by VARCHAR(255);
ALTER TABLE users ADD COLUMN IF NOT EXISTS department VARCHAR(50);

COMMENT ON COLUMN users.password_hash IS 'bcrypt hashed password';
COMMENT ON COLUMN users.force_password_change IS 'Force password change on next login';
COMMENT ON COLUMN users.password_changed_at IS 'Last password change timestamp';
COMMENT ON COLUMN users.login_attempts IS 'Failed login attempt counter';
COMMENT ON COLUMN users.locked_until IS 'Account locked until this timestamp';
COMMENT ON COLUMN users.last_login_at IS 'Last successful login timestamp';
COMMENT ON COLUMN users.last_login_ip IS 'Last login IP address';
COMMENT ON COLUMN users.email_verified IS 'Whether email has been verified';
COMMENT ON COLUMN users.created_by IS 'User who created this account';
COMMENT ON COLUMN users.department IS 'User department: cs, sales, tech, admin - assigned by admin only';

-- Index for email login (unique, case-insensitive)
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_unique ON users(LOWER(email)) WHERE email IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_username ON users(username) WHERE username IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_status_tenant ON users(status, tenant_id);

-- ============================================================
-- 2. Permissions table - Fine-grained permission definitions
-- ============================================================
CREATE TABLE IF NOT EXISTS permissions (
    id SERIAL PRIMARY KEY,
    permission_code VARCHAR(100) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    category VARCHAR(50) NOT NULL,  -- console, conversation, knowledge, user_management
    resource VARCHAR(100) NOT NULL,
    action VARCHAR(50) NOT NULL,
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE permissions IS 'Permission definitions - fine-grained permissions for all system features';
COMMENT ON COLUMN permissions.permission_code IS 'Unique permission code in format resource:action or category:resource:action';
COMMENT ON COLUMN permissions.category IS 'Permission category: console, conversation, knowledge, user_management';
COMMENT ON COLUMN permissions.resource IS 'Resource being accessed: dashboard, services, playground, dataset, user, role';
COMMENT ON COLUMN permissions.action IS 'Action on resource: view, edit, create, delete, access, manage';

CREATE INDEX IF NOT EXISTS idx_permissions_category ON permissions(category);
CREATE INDEX IF NOT EXISTS idx_permissions_resource ON permissions(resource);

-- Trigger for updated_at
DROP TRIGGER IF EXISTS update_permissions_updated_at ON permissions;
CREATE TRIGGER update_permissions_updated_at BEFORE UPDATE ON permissions
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- 3. Role permissions mapping table
-- ============================================================
CREATE TABLE IF NOT EXISTS role_permissions (
    id SERIAL PRIMARY KEY,
    role_name VARCHAR(100) NOT NULL,
    permission_code VARCHAR(100) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(role_name, permission_code)
);

COMMENT ON TABLE role_permissions IS 'Mapping between roles and permissions';

CREATE INDEX IF NOT EXISTS idx_role_permissions_role ON role_permissions(role_name);
CREATE INDEX IF NOT EXISTS idx_role_permissions_perm ON role_permissions(permission_code);

-- ============================================================
-- 4. User roles mapping table (many-to-many)
-- ============================================================
CREATE TABLE IF NOT EXISTS user_roles (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    role_name VARCHAR(100) NOT NULL,
    granted_by VARCHAR(255),
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    UNIQUE(user_id, role_name)
);

COMMENT ON TABLE user_roles IS 'Mapping between users and roles';
COMMENT ON COLUMN user_roles.granted_by IS 'User who granted this role';
COMMENT ON COLUMN user_roles.expires_at IS 'Optional expiration time for temporary roles';

CREATE INDEX IF NOT EXISTS idx_user_roles_user_id ON user_roles(user_id);
CREATE INDEX IF NOT EXISTS idx_user_roles_role_name ON user_roles(role_name);

-- ============================================================
-- 5. Login audit log table
-- ============================================================
CREATE TABLE IF NOT EXISTS login_audit (
    id BIGSERIAL PRIMARY KEY,
    user_id VARCHAR(255),
    email VARCHAR(255),
    action VARCHAR(50) NOT NULL,  -- login_success, login_failed, logout, password_change
    ip_address VARCHAR(45),
    user_agent TEXT,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE login_audit IS 'Audit log for login/logout events and password changes';
COMMENT ON COLUMN login_audit.action IS 'Action type: login_success, login_failed, logout, password_change, password_reset';

CREATE INDEX IF NOT EXISTS idx_login_audit_user_id ON login_audit(user_id);
CREATE INDEX IF NOT EXISTS idx_login_audit_email ON login_audit(email);
CREATE INDEX IF NOT EXISTS idx_login_audit_action ON login_audit(action);
CREATE INDEX IF NOT EXISTS idx_login_audit_created_at ON login_audit(created_at DESC);

-- ============================================================
-- 6. Insert default permissions
-- ============================================================
INSERT INTO permissions (permission_code, name, description, category, resource, action, is_system) VALUES
    -- Console permissions
    ('console:dashboard:view', 'View Dashboard', 'Access to dashboard overview', 'console', 'dashboard', 'view', TRUE),
    ('console:settings:view', 'View Settings', 'Access to settings page', 'console', 'settings', 'view', TRUE),
    ('console:settings:edit', 'Edit Settings', 'Modify system settings', 'console', 'settings', 'edit', TRUE),
    ('console:services:view', 'View Services', 'Access to services management', 'console', 'services', 'view', TRUE),
    ('console:services:edit', 'Edit Services', 'Modify service configurations', 'console', 'services', 'edit', TRUE),

    -- Conversation/Playground permissions
    ('conversation:playground:access', 'Access Playground', 'Access to AI conversation playground', 'conversation', 'playground', 'access', TRUE),
    ('conversation:thread:create', 'Create Thread', 'Create new conversation threads', 'conversation', 'thread', 'create', TRUE),
    ('conversation:thread:delete', 'Delete Thread', 'Delete conversation threads', 'conversation', 'thread', 'delete', TRUE),

    -- Knowledge base permissions
    ('knowledge:dataset:view', 'View Datasets', 'Access to knowledge base datasets', 'knowledge', 'dataset', 'view', TRUE),
    ('knowledge:dataset:create', 'Create Dataset', 'Create new datasets', 'knowledge', 'dataset', 'create', TRUE),
    ('knowledge:dataset:edit', 'Edit Dataset', 'Modify dataset configurations', 'knowledge', 'dataset', 'edit', TRUE),
    ('knowledge:dataset:delete', 'Delete Dataset', 'Delete datasets', 'knowledge', 'dataset', 'delete', TRUE),
    ('knowledge:document:upload', 'Upload Document', 'Upload documents to datasets', 'knowledge', 'document', 'upload', TRUE),
    ('knowledge:confluence:manage', 'Manage Confluence', 'Manage Confluence integrations', 'knowledge', 'confluence', 'manage', TRUE),

    -- User management permissions
    ('user:list', 'List Users', 'View user list', 'user_management', 'user', 'list', TRUE),
    ('user:create', 'Create User', 'Create new users', 'user_management', 'user', 'create', TRUE),
    ('user:edit', 'Edit User', 'Modify user information', 'user_management', 'user', 'edit', TRUE),
    ('user:delete', 'Delete User', 'Delete users', 'user_management', 'user', 'delete', TRUE),
    ('user:role:assign', 'Assign Roles', 'Assign roles to users', 'user_management', 'role', 'assign', TRUE),

    -- Role and permission management
    ('role:list', 'List Roles', 'View role list', 'user_management', 'role', 'list', TRUE),
    ('role:create', 'Create Role', 'Create new roles', 'user_management', 'role', 'create', TRUE),
    ('role:edit', 'Edit Role', 'Modify role permissions', 'user_management', 'role', 'edit', TRUE),
    ('role:delete', 'Delete Role', 'Delete roles', 'user_management', 'role', 'delete', TRUE),

    -- Admin wildcard (special permission)
    ('admin:*', 'Full Admin Access', 'Complete administrative access to all features', 'user_management', 'admin', '*', TRUE)
ON CONFLICT (permission_code) DO NOTHING;

-- ============================================================
-- 7. Update rbac_roles with new roles for internal users
-- ============================================================
-- Insert new roles
INSERT INTO rbac_roles (role_name, description, permissions, is_system) VALUES
    ('cs_staff', 'Customer Service Staff',
     ARRAY['console:dashboard:view', 'conversation:playground:access', 'conversation:thread:create', 'conversation:thread:delete', 'knowledge:dataset:view']::VARCHAR(100)[], TRUE),
    ('sales_staff', 'Sales Staff',
     ARRAY['console:dashboard:view', 'conversation:playground:access', 'conversation:thread:create', 'conversation:thread:delete', 'knowledge:dataset:view']::VARCHAR(100)[], TRUE),
    ('manager', 'Department Manager',
     ARRAY['console:dashboard:view', 'console:settings:view', 'console:services:view', 'conversation:playground:access', 'conversation:thread:create', 'conversation:thread:delete', 'knowledge:dataset:view', 'knowledge:dataset:create', 'knowledge:dataset:edit', 'user:list']::VARCHAR(100)[], TRUE)
ON CONFLICT (role_name) DO UPDATE SET
    description = EXCLUDED.description,
    permissions = EXCLUDED.permissions,
    updated_at = NOW();

-- Update existing admin role to have full access
UPDATE rbac_roles SET
    permissions = ARRAY['admin:*']::VARCHAR(100)[],
    description = 'System Administrator with full access to all features'
WHERE role_name = 'admin';

-- Update existing user role
UPDATE rbac_roles SET
    permissions = ARRAY['console:dashboard:view', 'conversation:playground:access', 'conversation:thread:create']::VARCHAR(100)[],
    description = 'Regular user with basic access'
WHERE role_name = 'user';

-- ============================================================
-- 8. Insert role_permissions mappings
-- ============================================================
-- cs_staff permissions
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'cs_staff', unnest(ARRAY[
    'console:dashboard:view',
    'conversation:playground:access',
    'conversation:thread:create',
    'conversation:thread:delete',
    'knowledge:dataset:view'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- sales_staff permissions
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'sales_staff', unnest(ARRAY[
    'console:dashboard:view',
    'conversation:playground:access',
    'conversation:thread:create',
    'conversation:thread:delete',
    'knowledge:dataset:view'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- manager permissions
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'manager', unnest(ARRAY[
    'console:dashboard:view',
    'console:settings:view',
    'console:services:view',
    'conversation:playground:access',
    'conversation:thread:create',
    'conversation:thread:delete',
    'knowledge:dataset:view',
    'knowledge:dataset:create',
    'knowledge:dataset:edit',
    'user:list'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- admin full access
INSERT INTO role_permissions (role_name, permission_code)
VALUES ('admin', 'admin:*')
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- user basic access
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'user', unnest(ARRAY[
    'console:dashboard:view',
    'conversation:playground:access',
    'conversation:thread:create'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- guest public access
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'guest', unnest(ARRAY[
    'console:dashboard:view'
])
ON CONFLICT (role_name, permission_code) DO NOTHING;

-- ============================================================
-- 9. Create local bootstrap admin user
-- Security note:
--   Do not commit or reapply a known bootstrap password here. Operators may
--   optionally pass a bcrypt hash through a PostgreSQL custom setting, for
--   example:
--     PGOPTIONS='-c gateway.bootstrap_admin_password_hash=<bcrypt-hash>'
--   If unset, the admin account is created without a login password and must be
--   initialized through the password reset/user-management flow.
-- ============================================================
INSERT INTO users (
    user_id,
    username,
    email,
    display_name,
    tenant_id,
    tier,
    roles,
    permissions,
    status,
    password_hash,
    force_password_change,
    email_verified,
    created_by
) VALUES (
    'admin',
    'admin',
    'admin@example.com',
    'System Administrator',
    'default',
    'admin',
    ARRAY['admin']::VARCHAR(50)[],
    ARRAY['admin:*']::VARCHAR(100)[],
    'active',
    NULLIF(current_setting('gateway.bootstrap_admin_password_hash', TRUE), ''),
    TRUE,
    TRUE,
    'system'
) ON CONFLICT (user_id) DO UPDATE SET
    email = EXCLUDED.email,
    display_name = EXCLUDED.display_name,
    tier = EXCLUDED.tier,
    roles = EXCLUDED.roles,
    permissions = EXCLUDED.permissions,
    email_verified = EXCLUDED.email_verified,
    updated_at = NOW();

-- Insert user_roles mapping for admin
INSERT INTO user_roles (user_id, role_name, granted_by)
VALUES ('admin', 'admin', 'system')
ON CONFLICT (user_id, role_name) DO NOTHING;

-- ============================================================
-- 10. Email domain configuration table (for future extensibility)
-- ============================================================
CREATE TABLE IF NOT EXISTS email_domain_config (
    id SERIAL PRIMARY KEY,
    domain VARCHAR(255) NOT NULL UNIQUE,
    is_allowed BOOLEAN NOT NULL DEFAULT TRUE,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE email_domain_config IS 'Allowed email domains for user registration';

-- Insert allowed domain
INSERT INTO email_domain_config (domain, is_allowed, description)
VALUES ('example.com', TRUE, 'Default local development email domain')
ON CONFLICT (domain) DO NOTHING;

-- Trigger for updated_at
DROP TRIGGER IF EXISTS update_email_domain_config_updated_at ON email_domain_config;
CREATE TRIGGER update_email_domain_config_updated_at BEFORE UPDATE ON email_domain_config
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ============================================================
-- 11. Password history table (for password reuse prevention - optional)
-- ============================================================
CREATE TABLE IF NOT EXISTS password_history (
    id SERIAL PRIMARY KEY,
    user_id VARCHAR(255) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE password_history IS 'Password history for preventing password reuse';

CREATE INDEX IF NOT EXISTS idx_password_history_user_id ON password_history(user_id);

-- ============================================================
-- Migration complete
-- ============================================================
