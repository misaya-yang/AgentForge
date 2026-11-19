-- 099 - Repair tenant-qualified memory uniqueness in upgraded Gateway schemas.
--
-- Older installations could record migration 026 against another search_path,
-- leaving gateway.session_memory and gateway.user_memory with their original
-- two-column unique constraints. Rust capability writes use tenant-qualified
-- ON CONFLICT targets and therefore fail closed until these constraints match
-- the authoritative schema.

BEGIN;

ALTER TABLE IF EXISTS gateway.session_memory
    DROP CONSTRAINT IF EXISTS session_memory_session_id_key_key;

DO $$
BEGIN
    IF to_regclass('gateway.session_memory') IS NOT NULL
       AND NOT EXISTS (
           SELECT 1
           FROM pg_constraint AS constraint_record
           JOIN pg_class AS relation
             ON relation.oid = constraint_record.conrelid
           JOIN pg_namespace AS namespace
             ON namespace.oid = relation.relnamespace
           WHERE namespace.nspname = 'gateway'
             AND relation.relname = 'session_memory'
             AND constraint_record.conname = 'session_memory_tenant_session_key_unique'
       ) THEN
        ALTER TABLE gateway.session_memory
            ADD CONSTRAINT session_memory_tenant_session_key_unique
            UNIQUE (tenant_id, session_id, key);
    END IF;
END $$;

ALTER TABLE IF EXISTS gateway.user_memory
    DROP CONSTRAINT IF EXISTS user_memory_user_id_key_key;

DO $$
BEGIN
    IF to_regclass('gateway.user_memory') IS NOT NULL
       AND NOT EXISTS (
           SELECT 1
           FROM pg_constraint AS constraint_record
           JOIN pg_class AS relation
             ON relation.oid = constraint_record.conrelid
           JOIN pg_namespace AS namespace
             ON namespace.oid = relation.relnamespace
           WHERE namespace.nspname = 'gateway'
             AND relation.relname = 'user_memory'
             AND constraint_record.conname = 'user_memory_tenant_user_key_unique'
       ) THEN
        ALTER TABLE gateway.user_memory
            ADD CONSTRAINT user_memory_tenant_user_key_unique
            UNIQUE (tenant_id, user_id, key);
    END IF;
END $$;

COMMIT;
