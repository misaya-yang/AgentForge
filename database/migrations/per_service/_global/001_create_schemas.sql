-- =============================================================================
-- Phase 6 — schema-per-service split.
-- =============================================================================
-- Idempotent. Safe to run multiple times. Run BEFORE 002_move_tables.sql.
--
-- Why: shared `public` schema = every service can read/write every table.
-- After this migration, `gateway`/`assistant`/`knowledge` schemas exist with
-- explicit ownership.
--
-- Run order across files:
--   _global/001_create_schemas.sql      ← this file: schemas + search_path
--   _global/002_move_tables.sql         ← moves tables from public to schemas
--   gateway/*.sql                       ← future gateway-only migrations
--   assistant/*.sql                     ← future assistant-only migrations
--   knowledge/*.sql                     ← future knowledge-only migrations
-- =============================================================================

CREATE SCHEMA IF NOT EXISTS gateway;
CREATE SCHEMA IF NOT EXISTS assistant;
CREATE SCHEMA IF NOT EXISTS knowledge;

-- Grants. The single DB role currently is `postgres` so grants are no-op
-- but written explicitly so a future per-service role split is one ALTER away.
GRANT USAGE, CREATE ON SCHEMA gateway, assistant, knowledge TO PUBLIC;

-- Default search_path keeps unqualified queries resolving during the
-- transition. Order is gateway, assistant, knowledge, public — public last
-- so anything still in public stays reachable.
-- ALTER DATABASE expects a literal identifier — wrap in DO block so the
-- runner resolves the current DB name at execution time. (psql does this
-- via :DBNAME substitution; asyncpg sends the raw SQL.)
DO $do$
BEGIN
    EXECUTE format(
        'ALTER DATABASE %I SET search_path = gateway, assistant, knowledge, public',
        current_database()
    );
END
$do$;

-- Per-session search_path for any role that opens a connection right now —
-- DB-level ALTER only takes effect on new sessions.
SET search_path = gateway, assistant, knowledge, public;

-- Migration ledger row to mark progress.
CREATE TABLE IF NOT EXISTS public.schema_migrations_meta (
    name        TEXT PRIMARY KEY,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    notes       TEXT
);
INSERT INTO public.schema_migrations_meta(name, notes)
VALUES ('phase6_schemas_created', 'gateway/assistant/knowledge schemas + search_path')
ON CONFLICT (name) DO NOTHING;
