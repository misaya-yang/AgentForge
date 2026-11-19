-- ============================================================================
-- AI Gateway role bootstrap (PRD ARC-03 §3D)
-- ============================================================================
-- ADMIN-ONLY: creates the platform's NOLOGIN object owner and LOGIN process
-- roles.  Idempotent by design; run by a one-time PostgreSQL admin (local
-- Compose) or pre-created by a DBA (managed PostgreSQL, receipt without
-- secrets).  ai_gateway_migrator cannot execute this file: it intentionally
-- has no ALTER ROLE / CREATE ROLE authority and only verifies the result.
--
-- Hard rules encoded here:
--   * ai_gateway_migrator is the only DDL executor and NEVER has CREATEROLE
--     or cluster-admin rights; it can SET ROLE into the NOLOGIN owner.
--   * Application roles get no CREATE rights and no broad PUBLIC grants.
--   * Role-level search_path starts with pg_catalog and ends with public.
--   * Passwords are never part of this file; credential generation is a
--     runtime input (scripts/new/init-env.sh / managed DBA), not repo state.
--
-- The canonical prefix is ``ai_gateway_``.  Managed deployments may
-- namespace roles through AI_GATEWAY_ROLE_PREFIX; the authority renders
-- this file with the configured prefix before executing it.
-- ============================================================================

-- 1. NOLOGIN object owner ---------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_owner') THEN
        CREATE ROLE ai_gateway_owner NOLOGIN;
    END IF;
END
$$;
ALTER ROLE ai_gateway_owner
    NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

-- 2. Migrator: sole DDL identity --------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_migrator') THEN
        CREATE ROLE ai_gateway_migrator LOGIN;
    END IF;
END
$$;
ALTER ROLE ai_gateway_migrator
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

-- 3. Application LOGIN roles -------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_gateway') THEN
        CREATE ROLE ai_gateway_gateway LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_runtime') THEN
        CREATE ROLE ai_gateway_runtime LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_capability_worker') THEN
        CREATE ROLE ai_gateway_capability_worker LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_knowledge_api') THEN
        CREATE ROLE ai_gateway_knowledge_api LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ai_gateway_knowledge_worker') THEN
        CREATE ROLE ai_gateway_knowledge_worker LOGIN;
    END IF;
END
$$;

ALTER ROLE ai_gateway_gateway
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE ai_gateway_runtime
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE ai_gateway_capability_worker
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE ai_gateway_knowledge_api
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE ai_gateway_knowledge_worker
    LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

-- Dedicated process roles inherit no pre-existing membership.  This closes a
-- real idempotency hole: changing rolcreaterole/rolsuper alone does not remove
-- privileges inherited from an older broad role.  The sole intended
-- membership is restored for the migrator immediately afterwards.
DO $$
DECLARE
    target_name TEXT;
    inherited_name TEXT;
BEGIN
    FOREACH target_name IN ARRAY ARRAY[
        'ai_gateway_owner',
        'ai_gateway_migrator',
        'ai_gateway_gateway',
        'ai_gateway_runtime',
        'ai_gateway_capability_worker',
        'ai_gateway_knowledge_api',
        'ai_gateway_knowledge_worker'
    ]
    LOOP
        FOR inherited_name IN
            SELECT granted.rolname
            FROM pg_auth_members AS membership
            JOIN pg_roles AS member ON member.oid = membership.member
            JOIN pg_roles AS granted ON granted.oid = membership.roleid
            WHERE member.rolname = target_name
        LOOP
            EXECUTE format('REVOKE %I FROM %I', inherited_name, target_name);
        END LOOP;
    END LOOP;
END
$$;

-- The migrator applies DDL only by becoming the NOLOGIN owner; it never owns
-- platform objects permanently and receives no direct schema CREATE grant.
GRANT ai_gateway_owner TO ai_gateway_migrator;

-- 4. Role search_path: pg_catalog first, public last, no CREATE anywhere ----
ALTER ROLE ai_gateway_owner SET search_path = pg_catalog, gateway, assistant, knowledge, public;
ALTER ROLE ai_gateway_migrator SET search_path = pg_catalog, gateway, assistant, knowledge, public;
ALTER ROLE ai_gateway_gateway SET search_path = pg_catalog, gateway, assistant, knowledge, public;
ALTER ROLE ai_gateway_runtime SET search_path = pg_catalog, gateway, assistant, knowledge, public;
ALTER ROLE ai_gateway_capability_worker SET search_path = pg_catalog, gateway, assistant, knowledge, public;
ALTER ROLE ai_gateway_knowledge_api SET search_path = pg_catalog, knowledge, gateway, assistant, public;
ALTER ROLE ai_gateway_knowledge_worker SET search_path = pg_catalog, knowledge, gateway, assistant, public;

-- 5. Stable schema ownership -------------------------------------------------
-- Fresh baseline DDL runs after SET ROLE ai_gateway_owner.  Owning each schema
-- gives that NOLOGIN identity CREATE without granting CREATE to any process
-- LOGIN role.  On an existing database this is the first, transactional step
-- of convergence; cutover_convergence.sql repeats and verifies the full set.
CREATE SCHEMA IF NOT EXISTS gateway AUTHORIZATION ai_gateway_owner;
CREATE SCHEMA IF NOT EXISTS assistant AUTHORIZATION ai_gateway_owner;
CREATE SCHEMA IF NOT EXISTS knowledge AUTHORIZATION ai_gateway_owner;

ALTER SCHEMA public OWNER TO ai_gateway_owner;
ALTER SCHEMA gateway OWNER TO ai_gateway_owner;
ALTER SCHEMA assistant OWNER TO ai_gateway_owner;
ALTER SCHEMA knowledge OWNER TO ai_gateway_owner;

REVOKE ALL ON SCHEMA public, gateway, assistant, knowledge FROM PUBLIC;
REVOKE CREATE ON SCHEMA public, gateway, assistant, knowledge FROM
    ai_gateway_migrator,
    ai_gateway_gateway,
    ai_gateway_runtime,
    ai_gateway_capability_worker,
    ai_gateway_knowledge_api,
    ai_gateway_knowledge_worker;

-- Trusted extension installation needs CREATE on this database. Keep it on
-- the NOLOGIN owner only: the migrator must explicitly SET ROLE, while
-- application roles and PUBLIC cannot create schemas or extensions.
DO $$
BEGIN
    EXECUTE format('REVOKE CREATE ON DATABASE %I FROM PUBLIC', current_database());
    EXECUTE format(
        'REVOKE CREATE ON DATABASE %I FROM '
        'ai_gateway_migrator, ai_gateway_gateway, ai_gateway_runtime, '
        'ai_gateway_capability_worker, ai_gateway_knowledge_api, '
        'ai_gateway_knowledge_worker',
        current_database()
    );
    EXECUTE format(
        'GRANT CREATE ON DATABASE %I TO ai_gateway_owner', current_database()
    );
END
$$;

-- 6. Safe defaults for baseline and future epoch objects ---------------------
-- PostgreSQL grants PUBLIC EXECUTE on new routines and PUBLIC USAGE on new
-- types unless the creating role overrides those defaults.  Baseline DDL runs
-- as ai_gateway_owner, so freeze the four schemas before any object exists.
-- The built-in grants are global defaults; schema-specific REVOKE cannot
-- subtract them. Revoke globally first, then retain explicit per-schema rows
-- so the fingerprint proves the intended scope.
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA public
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA gateway
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA assistant
    REVOKE USAGE ON TYPES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE ALL ON SEQUENCES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE EXECUTE ON ROUTINES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA knowledge
    REVOKE USAGE ON TYPES FROM PUBLIC;
