-- =============================================================================
-- Migration 063 — Seed Eval console permissions and grant to developer/manager
-- =============================================================================
-- console:eval:view  — read agent traces, datasets, experiments (Eval Console)
-- console:eval:run — score traces, run evaluators, mutate eval artifacts
--
-- Idempotent: ON CONFLICT DO NOTHING / array merge via UPDATE.
-- =============================================================================

INSERT INTO permissions (permission_code, name, description, category, resource, action, is_system)
VALUES
    (
        'console:eval:view',
        'View Eval Console',
        'Access to agent trace evaluation console and trace queries',
        'console',
        'eval',
        'view',
        TRUE
    ),
    (
        'console:eval:run',
        'Run Eval Operations',
        'Score traces, run evaluators, and manage eval datasets and experiments',
        'console',
        'eval',
        'run',
        TRUE
    )
ON CONFLICT (permission_code) DO NOTHING;

-- developer: full eval access
INSERT INTO role_permissions (role_name, permission_code)
SELECT 'developer', unnest(ARRAY['console:eval:view', 'console:eval:run']::VARCHAR(100)[])
ON CONFLICT (role_name, permission_code) DO NOTHING;

UPDATE rbac_roles
SET
    permissions = (
        SELECT ARRAY(
            SELECT DISTINCT unnest(
                permissions || ARRAY['console:eval:view', 'console:eval:run']::VARCHAR(100)[]
            )
        )
    ),
    updated_at = NOW()
WHERE role_name = 'developer';

-- manager: trace monitoring (view only)
INSERT INTO role_permissions (role_name, permission_code)
VALUES ('manager', 'console:eval:view')
ON CONFLICT (role_name, permission_code) DO NOTHING;

UPDATE rbac_roles
SET
    permissions = (
        SELECT ARRAY(
            SELECT DISTINCT unnest(
                permissions || ARRAY['console:eval:view']::VARCHAR(100)[]
            )
        )
    ),
    updated_at = NOW()
WHERE role_name = 'manager';