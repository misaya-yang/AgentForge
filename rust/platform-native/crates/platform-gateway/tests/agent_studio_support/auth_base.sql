-- Disposable gateway-schema test database only. Columns consumed by native admission.
ALTER TABLE users ADD COLUMN email text, ADD COLUMN display_name text, ADD COLUMN department text,
 ADD COLUMN tier text NOT NULL DEFAULT 'free', ADD COLUMN permissions text[] NOT NULL DEFAULT '{}',
 ADD COLUMN password_hash text, ADD COLUMN force_password_change boolean NOT NULL DEFAULT false,
 ADD COLUMN login_attempts integer NOT NULL DEFAULT 0, ADD COLUMN locked_until timestamptz,
 ADD COLUMN password_changed_at timestamptz;
CREATE TABLE tenants(tenant_id text PRIMARY KEY,status text NOT NULL DEFAULT 'active');
CREATE TABLE user_roles(user_id text,role_name text,expires_at timestamptz);
CREATE TABLE role_permissions(role_name text,permission_code text);
CREATE TABLE rbac_roles(role_name text,permissions text[] NOT NULL DEFAULT '{}');
CREATE TABLE user_permissions(user_id text,permission_code text,expires_at timestamptz);
