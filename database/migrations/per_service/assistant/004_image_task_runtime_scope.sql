-- Per-service owner for the image-task runtime scope contract.
--
-- Root migration 097 safely skips when assistant.image_tasks has not been
-- installed yet. assistant/002 creates that table later in the schema-split
-- chain; this migration then applies the same additive constraint and indexes.

ALTER TABLE assistant.image_tasks
    ADD COLUMN IF NOT EXISTS tenant_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS user_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS runtime_scope_version SMALLINT NOT NULL DEFAULT 0;

DO $migration$
BEGIN
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
END
$migration$;

CREATE INDEX IF NOT EXISTS idx_image_tasks_runtime_queue
    ON assistant.image_tasks(status, locked_until, created_at)
    WHERE runtime_scope_version = 1
      AND status IN ('pending', 'running');

CREATE INDEX IF NOT EXISTS idx_image_tasks_runtime_owner
    ON assistant.image_tasks(tenant_id, user_id, created_at DESC)
    WHERE runtime_scope_version = 1;

COMMENT ON COLUMN assistant.image_tasks.runtime_scope_version IS
    '0=legacy opaque owner only; 1=explicit tenant/user scope safe for worker recovery';
