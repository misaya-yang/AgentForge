-- T3 (docs/plans/rag-upgrade-prd-2026-08.md §T3/§6.3): embedding versioning
-- metadata + the blue-green collection-binding indirection. Central-ledger
-- migration (root chain only, PRD §9). Idempotent; additive only.
--
-- Contract implemented here (owned by
-- knowledge_service/persistence/embedding_version_store.py +
-- services/knowledge/embedding_migration.py):
--   * version metadata: datasets carry embedding_model_version alongside the
--     pre-existing provider/model/dimension columns; documents carry the
--     embedding identity generation that last served their vectors.
--   * the 082 1:1 datasets.collection_name column is upgraded to a 1:N
--     binding table with an ACTIVE (state='serving') row per dataset — the
--     "活动别名" indirection (logical KB -> physical collection). The 082
--     unique constraint on datasets stays in force untouched; bindings add
--     the same reservation semantics for collections that are not (yet)
--     written back to a datasets row (shadow generations).
--   * migration jobs (shadow build -> backfill -> verify -> gate -> ready ->
--     cutover) with a resumable per-chunk progress ledger: authoritative
--     source is the PostgreSQL enabled-chunk rows, never Qdrant self-witness.
--   * rollback path: cutover demotes the old binding to 'retained' (never
--     deleted); rollback flips it back to 'serving' in one transaction.
--   * capability flags ride the binding row (addendum §1-T3.2) so a
--     blue-green migration carries them forward atomically.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

-- 1. Version metadata on the dataset identity.
ALTER TABLE datasets
    ADD COLUMN IF NOT EXISTS embedding_model_version VARCHAR(100) NOT NULL DEFAULT '';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'datasets'::regclass
          AND conname = 'uq_datasets_identity_tenant'
    ) THEN
        ALTER TABLE datasets
            ADD CONSTRAINT uq_datasets_identity_tenant
            UNIQUE (dataset_id, tenant_id);
    END IF;
END
$$;

COMMENT ON COLUMN datasets.embedding_model_version IS
  'T3: server-owned embedding model version; completes (provider, model, dimension) as the embedding identity';

-- 2. Per-document embedding provenance. Nullable dimension keeps legacy rows
--    honest ('unknown') instead of inventing a value for them.
ALTER TABLE documents
    ADD COLUMN IF NOT EXISTS embedding_model VARCHAR(100) NOT NULL DEFAULT '';
ALTER TABLE documents
    ADD COLUMN IF NOT EXISTS embedding_model_version VARCHAR(100) NOT NULL DEFAULT '';
ALTER TABLE documents
    ADD COLUMN IF NOT EXISTS embedding_dimension INTEGER;

COMMENT ON COLUMN documents.embedding_model IS
  'T3: embedding identity that last served this document''s vectors (written by backfill/reembed paths)';

-- 3. Collection binding indirection: logical dataset -> physical collection.
CREATE TABLE IF NOT EXISTS dataset_collection_bindings (
    binding_id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dataset_id        VARCHAR(255) NOT NULL,
    tenant_id         VARCHAR(255) NOT NULL DEFAULT '',
    collection_name   VARCHAR(255) NOT NULL,
    embedding_provider VARCHAR(50) NOT NULL DEFAULT '',
    embedding_model   VARCHAR(100) NOT NULL DEFAULT '',
    embedding_model_version VARCHAR(100) NOT NULL DEFAULT '',
    embedding_dimension INTEGER NOT NULL CHECK (embedding_dimension > 0),
    capabilities      JSONB NOT NULL DEFAULT '[]'::jsonb,
    state             VARCHAR(20) NOT NULL DEFAULT 'shadow'
        CHECK (state IN ('shadow', 'serving', 'retained', 'retired')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    activated_at      TIMESTAMPTZ,
    retired_at        TIMESTAMPTZ,
    retained_until    TIMESTAMPTZ,
    CONSTRAINT fk_kb_binding_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT chk_kb_binding_capabilities_array
        CHECK (jsonb_typeof(capabilities) = 'array')
);

COMMENT ON TABLE dataset_collection_bindings IS
  'T3 blue-green: physical collection generations per dataset; exactly one serving binding';
COMMENT ON COLUMN dataset_collection_bindings.capabilities IS
  'addendum §1-T3.2: embedding capability flags (e.g. vision) that travel with the binding across migrations';

-- At most one serving binding per dataset (the "active alias").
CREATE UNIQUE INDEX IF NOT EXISTS idx_kb_bindings_one_serving_per_dataset
    ON dataset_collection_bindings (dataset_id)
    WHERE state = 'serving';

-- Reservation semantics of 082 extended to non-datasets collections: a live
-- (not retired) collection name can never be claimed by a second binding.
CREATE UNIQUE INDEX IF NOT EXISTS idx_kb_bindings_collection_name_live_unique
    ON dataset_collection_bindings (collection_name)
    WHERE state <> 'retired';

CREATE INDEX IF NOT EXISTS idx_kb_bindings_dataset_state
    ON dataset_collection_bindings (dataset_id, state);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'dataset_collection_bindings'::regclass
          AND conname = 'uq_kb_binding_dataset_identity'
    ) THEN
        ALTER TABLE dataset_collection_bindings
            ADD CONSTRAINT uq_kb_binding_dataset_identity
            UNIQUE (binding_id, dataset_id);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'dataset_collection_bindings'::regclass
          AND conname = 'fk_kb_binding_dataset_tenant'
    ) THEN
        ALTER TABLE dataset_collection_bindings
            ADD CONSTRAINT fk_kb_binding_dataset_tenant
            FOREIGN KEY (dataset_id, tenant_id)
            REFERENCES datasets (dataset_id, tenant_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'dataset_collection_bindings'::regclass
          AND conname = 'chk_kb_binding_capabilities_array'
    ) THEN
        ALTER TABLE dataset_collection_bindings
            ADD CONSTRAINT chk_kb_binding_capabilities_array
            CHECK (jsonb_typeof(capabilities) = 'array')
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE dataset_collection_bindings
    VALIDATE CONSTRAINT fk_kb_binding_dataset_tenant;
ALTER TABLE dataset_collection_bindings
    VALIDATE CONSTRAINT chk_kb_binding_capabilities_array;

-- 4. Blue-green migration jobs (one dataset generation moving from a source
--    binding to a target binding). Terminal states: completed, rolled_back,
--    failed, abandoned.
CREATE TABLE IF NOT EXISTS embedding_migrations (
    migration_id      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dataset_id        VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    source_binding_id UUID REFERENCES dataset_collection_bindings(binding_id) ON DELETE SET NULL,
    target_binding_id UUID NOT NULL REFERENCES dataset_collection_bindings(binding_id) ON DELETE CASCADE,
    state             VARCHAR(20) NOT NULL DEFAULT 'shadow_build'
        CHECK (state IN ('shadow_build', 'backfilling', 'verified', 'gating',
                         'gate_failed', 'ready', 'completed', 'rolled_back',
                         'failed', 'abandoned')),
    checkpoint        JSONB NOT NULL DEFAULT '{}'::jsonb,
    totals            JSONB NOT NULL DEFAULT '{}'::jsonb,
    gate              JSONB,
    error             TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE embedding_migrations IS
  'T3: shadow-build -> backfill -> verify -> eval-gate -> ready -> cutover job ledger';

-- One live migration per dataset; the old serving collection keeps serving
-- throughout (PRD §6.3 zero-window rule).
CREATE UNIQUE INDEX IF NOT EXISTS idx_kb_embedding_migrations_one_live_per_dataset
    ON embedding_migrations (dataset_id)
    WHERE state IN ('shadow_build', 'backfilling', 'verified', 'gating', 'ready');

CREATE INDEX IF NOT EXISTS idx_kb_embedding_migrations_state
    ON embedding_migrations (state);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migrations'::regclass
          AND conname = 'fk_kb_embedding_source_dataset'
    ) THEN
        ALTER TABLE embedding_migrations
            ADD CONSTRAINT fk_kb_embedding_source_dataset
            FOREIGN KEY (source_binding_id, dataset_id)
            REFERENCES dataset_collection_bindings (binding_id, dataset_id)
            ON DELETE SET NULL (source_binding_id)
            NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migrations'::regclass
          AND conname = 'fk_kb_embedding_target_dataset'
    ) THEN
        ALTER TABLE embedding_migrations
            ADD CONSTRAINT fk_kb_embedding_target_dataset
            FOREIGN KEY (target_binding_id, dataset_id)
            REFERENCES dataset_collection_bindings (binding_id, dataset_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'embedding_migrations'::regclass
          AND conname = 'chk_kb_embedding_distinct_bindings'
    ) THEN
        ALTER TABLE embedding_migrations
            ADD CONSTRAINT chk_kb_embedding_distinct_bindings
            CHECK (source_binding_id IS NULL OR source_binding_id <> target_binding_id)
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE embedding_migrations
    VALIDATE CONSTRAINT fk_kb_embedding_source_dataset;
ALTER TABLE embedding_migrations
    VALIDATE CONSTRAINT fk_kb_embedding_target_dataset;
ALTER TABLE embedding_migrations
    VALIDATE CONSTRAINT chk_kb_embedding_distinct_bindings;

-- 5. Resumable per-chunk progress ledger. A row exists iff the target
--    collection provably carries the vector for that segment's content hash
--    (skip-on-restart compares like with like; addendum: the corpus, not
--    Qdrant, decides completeness).
CREATE TABLE IF NOT EXISTS embedding_migration_progress (
    migration_id  UUID NOT NULL REFERENCES embedding_migrations(migration_id) ON DELETE CASCADE,
    segment_id    VARCHAR(255) NOT NULL,
    document_id   VARCHAR(255) NOT NULL,
    position      INTEGER NOT NULL,
    vector_id     VARCHAR(255) NOT NULL,
    content_hash  VARCHAR(64) NOT NULL,
    written_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (migration_id, segment_id)
);

CREATE INDEX IF NOT EXISTS idx_kb_embedding_migration_progress_doc
    ON embedding_migration_progress (migration_id, document_id, position);

-- 6. Content-hash-keyed embedding cache (PRD T3 item 4). The vector identity
--    is part of the key, so a cached vector can never be replayed under a
--    different model; values are float8[] (JSON-friendly), never pickle.
--    Batch lookups go through `WHERE content_hash = ANY(...)` in
--    EmbeddingVersionStore.lookup_embeddings_batch.
CREATE TABLE IF NOT EXISTS embedding_vector_cache (
    embedding_provider        VARCHAR(50) NOT NULL,
    embedding_model           VARCHAR(100) NOT NULL,
    embedding_model_version   VARCHAR(100) NOT NULL DEFAULT '',
    content_hash              VARCHAR(64) NOT NULL,
    vector                    DOUBLE PRECISION[] NOT NULL,
    dimension                 INTEGER NOT NULL CHECK (dimension > 0),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (embedding_provider, embedding_model, embedding_model_version, content_hash)
);

-- 7. Backfill bindings for existing datasets (idempotent seed of the
--    indirection layer). Soft-deleted datasets are skipped: their 082
--    reservation stays on the datasets row itself.
INSERT INTO dataset_collection_bindings (
    dataset_id, tenant_id, collection_name,
    embedding_provider, embedding_model, embedding_model_version,
    embedding_dimension, state, activated_at
)
SELECT
    d.dataset_id, d.tenant_id, d.collection_name,
    d.embedding_provider, d.embedding_model, d.embedding_model_version,
    GREATEST(COALESCE(d.embedding_dimension, 0), 1), 'serving', d.created_at
FROM datasets d
WHERE d.collection_name IS NOT NULL
  AND BTRIM(d.collection_name) <> ''
  AND d.is_deleted = FALSE
ON CONFLICT (dataset_id) WHERE state = 'serving' DO NOTHING;

COMMIT;
