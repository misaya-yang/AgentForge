// Exact SQL from Python legacy authority; no runtime Python dependency.
pub const BASE_SCHEMA_PROBE: &str = r####"
SELECT CASE WHEN
    COALESCE(to_regclass('gateway.services'), to_regclass('public.services')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.datasets'), to_regclass('public.datasets')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.documents'), to_regclass('public.documents')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.segments'), to_regclass('public.segments')) IS NOT NULL
THEN TRUE ELSE FALSE END
"####;
pub const NON_TRANSACTIONAL_INDEX_STATE_SQL: &str = r####"
/* arc03-legacy-049:index-state */
SELECT n.nspname AS schema_name, i.indisvalid, i.indisready,
       pg_get_indexdef(i.indexrelid) AS definition
FROM pg_index AS i
JOIN pg_class AS ic ON ic.oid = i.indexrelid
JOIN pg_namespace AS n ON n.oid = ic.relnamespace
JOIN pg_class AS tc ON tc.oid = i.indrelid
JOIN pg_namespace AS tn ON tn.oid = tc.relnamespace
WHERE n.nspname = tn.nspname
  AND n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
  AND ic.relname = 'idx_sessions_user_tenant_status_updated'
  AND tc.relname = 'sessions'
ORDER BY n.nspname
"####;
pub const NON_TRANSACTIONAL_049_POSTCHECK_SQL: &str = r####"
/* arc03-legacy-049:postcheck */
WITH matches AS (
    SELECT i.indisvalid, i.indisready, i.indisunique, i.indpred,
           i.indnkeyatts, i.indnatts, pg_get_indexdef(i.indexrelid) AS definition
    FROM pg_index AS i
    JOIN pg_class AS ic ON ic.oid = i.indexrelid
    JOIN pg_namespace AS n ON n.oid = ic.relnamespace
    JOIN pg_class AS tc ON tc.oid = i.indrelid
    JOIN pg_namespace AS tn ON tn.oid = tc.relnamespace
    WHERE n.nspname = tn.nspname
      AND n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
      AND ic.relname = 'idx_sessions_user_tenant_status_updated'
      AND tc.relname = 'sessions'
)
SELECT count(*) = 1
   AND bool_and(indisvalid AND indisready AND NOT indisunique)
   AND bool_and(indpred IS NULL AND indnkeyatts = 4 AND indnatts = 4)
   AND bool_and(
       definition ~
       '\(user_id, tenant_id, status, updated_at DESC\)$'
   )
FROM matches
"####;
