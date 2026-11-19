-- Prevent multiple datasets from binding to the same non-empty Qdrant
-- collection. Soft-deleted datasets remain reservations so a collection
-- cannot be silently reassigned across tenants.
--
-- Operational preflight (read-only): this query must return no rows before
-- applying the migration. If it returns rows, stop and resolve ownership
-- rather than choosing a winner automatically:
--
-- SELECT collection_name, COUNT(*) AS dataset_count
-- FROM knowledge.datasets
-- WHERE collection_name IS NOT NULL AND BTRIM(collection_name) <> ''
-- GROUP BY collection_name
-- HAVING COUNT(*) > 1;
--
-- PostgreSQL will abort and roll back this migration if a duplicate remains.
-- The per-service runner already wraps this file in one transaction.

CREATE UNIQUE INDEX IF NOT EXISTS idx_datasets_collection_name_unique_nonempty
    ON knowledge.datasets (collection_name)
    WHERE collection_name IS NOT NULL
      AND BTRIM(collection_name) <> '';
