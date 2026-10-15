-- =============================================================================
-- Migration 057 — Add model_tester role for playground-only access
-- =============================================================================
-- A dedicated role that grants ONLY conversation:playground:access.
-- Users assigned this role see nothing except the model playground/experience
-- page in the console. Useful for external testers or demo accounts.
--
-- Idempotent: ON CONFLICT DO UPDATE ensures safe re-runs.
-- =============================================================================

INSERT INTO rbac_roles (role_name, description, permissions, is_system)
VALUES (
    'model_tester',
    'Model Playground Tester — only playground access',
    ARRAY['conversation:playground:access']::VARCHAR(100)[],
    TRUE
)
ON CONFLICT (role_name) DO UPDATE SET
    description = EXCLUDED.description,
    permissions = EXCLUDED.permissions,
    is_system   = EXCLUDED.is_system,
    updated_at  = NOW();

-- Sync the many-to-many role_permissions ledger (defensive —
-- get_user_permissions reads role_permissions first, then falls back
-- to rbac_roles.permissions array, but keeping both in sync avoids
-- any edge-case drift).
INSERT INTO role_permissions (role_name, permission_code)
VALUES ('model_tester', 'conversation:playground:access')
ON CONFLICT (role_name, permission_code) DO NOTHING;
