-- 097 - Recoverable scope for Gateway-owned image task workers.
--
-- owner_scope is deliberately opaque and cannot be reversed into tenant/user
-- identity.  New workers claim only revision-1 rows with explicit identities;
-- existing rows remain revision 0 and are never guessed or cross-scoped.

BEGIN;

DO $migration$
BEGIN
    -- Fresh central-chain databases do not create assistant.image_tasks; that
    -- table belongs to per_service/assistant/002. The root chain must remain
    -- runnable before the optional schema-split chain, so absence is a
    -- deliberate no-op rather than an undefined-table failure.
    IF to_regclass('assistant.image_tasks') IS NULL THEN
        RAISE NOTICE
            '097_image_task_runtime_scope skipped: assistant.image_tasks is not installed';
        RETURN;
    END IF;

    ALTER TABLE assistant.image_tasks
        ADD COLUMN IF NOT EXISTS tenant_id VARCHAR(64),
        ADD COLUMN IF NOT EXISTS user_id VARCHAR(64),
        ADD COLUMN IF NOT EXISTS runtime_scope_version SMALLINT NOT NULL DEFAULT 0;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'assistant_image_tasks_runtime_scope_check'
           AND conrelid = 'assistant.image_tasks'::regclass
    ) THEN
        ALTER TABLE assistant.image_tasks
            ADD CONSTRAINT assistant_image_tasks_runtime_scope_check CHECK (
                runtime_scope_version = 0
                OR (
                    runtime_scope_version = 1
                    AND tenant_id IS NOT NULL AND tenant_id <> ''
                    AND user_id IS NOT NULL AND user_id <> ''
                )
            );
    END IF;

    CREATE INDEX IF NOT EXISTS idx_image_tasks_runtime_queue
        ON assistant.image_tasks(status, locked_until, created_at)
        WHERE runtime_scope_version = 1
          AND status IN ('pending', 'running');

    CREATE INDEX IF NOT EXISTS idx_image_tasks_runtime_owner
        ON assistant.image_tasks(tenant_id, user_id, created_at DESC)
        WHERE runtime_scope_version = 1;

    COMMENT ON COLUMN assistant.image_tasks.runtime_scope_version IS
        '0=legacy opaque owner only; 1=explicit tenant/user scope safe for worker recovery';
END
$migration$;

COMMIT;
