-- 093 — Bind Agent runtime ownership to the authoritative Assistant sessions.
--
-- The multi-schema deployment keeps user-facing sessions in assistant.sessions,
-- while the Gateway-owned Agent runtime tables live in gateway. Migration 089
-- originally resolved the unqualified sessions table through the database-wide
-- search_path and therefore referenced gateway.sessions. Rebind both ownership
-- foreign keys to the session repository's authoritative table.

BEGIN;

DO $$
BEGIN
    -- Single-schema contract tests intentionally do not create the production
    -- schemas. In that topology migration 089 already binds the correct table.
    IF to_regclass('assistant.sessions') IS NULL
       OR to_regclass('gateway.assistant_session_runtime_assignments') IS NULL
       OR to_regclass('gateway.assistant_runtime_threads') IS NULL THEN
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'sessions_runtime_owner_scope_key'
           AND conrelid = 'assistant.sessions'::regclass
    ) THEN
        ALTER TABLE assistant.sessions
            ADD CONSTRAINT sessions_runtime_owner_scope_key
            UNIQUE (tenant_id, user_id, session_id);
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'assistant_session_runtime_assignments_session_fk'
           AND conrelid = 'gateway.assistant_session_runtime_assignments'::regclass
           AND confrelid = 'assistant.sessions'::regclass
    ) THEN
        ALTER TABLE gateway.assistant_session_runtime_assignments
            DROP CONSTRAINT IF EXISTS assistant_session_runtime_assignments_session_fk;
        ALTER TABLE gateway.assistant_session_runtime_assignments
            ADD CONSTRAINT assistant_session_runtime_assignments_session_fk
            FOREIGN KEY (tenant_id, user_id, session_id)
            REFERENCES assistant.sessions(tenant_id, user_id, session_id)
            ON DELETE CASCADE;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'assistant_runtime_threads_session_fk'
           AND conrelid = 'gateway.assistant_runtime_threads'::regclass
           AND confrelid = 'assistant.sessions'::regclass
    ) THEN
        ALTER TABLE gateway.assistant_runtime_threads
            DROP CONSTRAINT IF EXISTS assistant_runtime_threads_session_fk;
        ALTER TABLE gateway.assistant_runtime_threads
            ADD CONSTRAINT assistant_runtime_threads_session_fk
            FOREIGN KEY (tenant_id, user_id, session_id)
            REFERENCES assistant.sessions(tenant_id, user_id, session_id)
            ON DELETE RESTRICT;
    END IF;
END;
$$;

COMMIT;
