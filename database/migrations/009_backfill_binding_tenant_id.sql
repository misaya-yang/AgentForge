-- ============================================================
-- Migration: 009_backfill_binding_tenant_id
-- Description: Backfill tenant_id for existing confluence_space_bindings
-- Version: 2.3.2
-- ============================================================

SET client_encoding TO 'UTF8';

-- Backfill tenant_id from confluence_connections for existing bindings
-- This ensures old bindings created before migration 007 are properly associated with tenants
UPDATE confluence_space_bindings b
SET tenant_id = c.tenant_id
FROM confluence_connections c
WHERE b.connection_id = c.connection_id
  AND b.tenant_id IS NULL;

-- ============================================================
-- Migration complete
-- ============================================================
