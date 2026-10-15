-- T6 (docs/plans/rag-upgrade-prd-2026-08.md §T6): BM25 v2 cross-storage
-- lifecycle protocol. Central-ledger migration (root chain only, PRD §9).
-- Idempotent; additive only — this table never carries lexical_v1 data and
-- dropping nothing here is required for a rollback.
--
-- Contract (owned by knowledge_service/persistence/bm25_v2_lifecycle.py +
-- services/knowledge/bm25_v2_lifecycle.py):
--   * Lifecycle state lives in this persistent table. The addendum forbids
--     Redis TTL flags/locks; the only lock in the protocol is a
--     PostgreSQL advisory lock keyed to the same namespace as the dataset
--     index write lease (pg_try_advisory_xact_lock_shared), which fails
--     every Qdrant writer closed for the duration of a transition — it is
--     not a TTL.
--   * One row per dataset that has entered the lexical rollout protocol.
--     Datasets that never entered have no row (baseline: no v2 profile).
--   * Steady states ('shadow', 'active_v2') carry no transition_kind and
--     no lock_token. In-progress states carry both; the token is a random
--     CAS credential proving the caller holds the barrier, and epoch is
--     bumped on every state change so a dead executor's late write can
--     never land on a newer row (crash recovery: a new executor takes the
--     advisory barrier first, so any in-progress row it finds is by
--     definition stale and safe to reset via fail_transition()).
--   * pre_evidence/post_evidence capture the two-phase confirmation of a
--     cutover: the completed backfill receipt verified before the flip
--     (authority content revision, point-ID/source-text digests) and the
--     recomputed agreement after the flip. Rollback records only what it
--     reverted; it is always available (v2 field and data are retained).

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

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

CREATE TABLE IF NOT EXISTS kb_bm25_v2_lifecycle (
    dataset_id      VARCHAR(255) PRIMARY KEY,
    tenant_id       VARCHAR(255) NOT NULL,
    state           VARCHAR(32) NOT NULL DEFAULT 'shadow'
        CHECK (state IN
            ('shadow', 'cutover_in_progress', 'active_v2', 'rollback_in_progress')),
    epoch           BIGINT NOT NULL DEFAULT 0,
    transition_kind VARCHAR(16)
        CHECK (transition_kind IN ('cutover', 'rollback')),
    lock_token      VARCHAR(64),
    authority_content_revision BIGINT,
    manifest_sha256 VARCHAR(80) NOT NULL DEFAULT '',
    pre_evidence    JSONB NOT NULL DEFAULT '{}'::jsonb,
    post_evidence   JSONB NOT NULL DEFAULT '{}'::jsonb,
    last_error      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT kb_bm25_v2_lifecycle_transition_shape CHECK (
        (state IN ('cutover_in_progress', 'rollback_in_progress')
            AND transition_kind IS NOT NULL
            AND lock_token IS NOT NULL
            AND epoch > 0)
        OR
        (state IN ('shadow', 'active_v2')
            AND transition_kind IS NULL
            AND lock_token IS NULL)
    ),
    CONSTRAINT fk_kb_bm25_lifecycle_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'kb_bm25_v2_lifecycle'::regclass
          AND conname = 'fk_kb_bm25_lifecycle_dataset_tenant'
    ) THEN
        ALTER TABLE kb_bm25_v2_lifecycle
            ADD CONSTRAINT fk_kb_bm25_lifecycle_dataset_tenant
            FOREIGN KEY (dataset_id, tenant_id)
            REFERENCES datasets (dataset_id, tenant_id)
            ON DELETE CASCADE
            NOT VALID;
    END IF;
END
$$;
ALTER TABLE kb_bm25_v2_lifecycle
    VALIDATE CONSTRAINT fk_kb_bm25_lifecycle_dataset_tenant;

COMMENT ON TABLE kb_bm25_v2_lifecycle IS
    'PRD T6: durable per-dataset BM25 v2 cutover/rollback lifecycle state + evidence (never a cache; writers are fenced by the dataset-index advisory lock, not by this row alone)';
COMMENT ON COLUMN kb_bm25_v2_lifecycle.epoch IS
    'monotonic CAS counter bumped on every state transition; late writers from a dead executor are rejected by (epoch, lock_token) match';
COMMENT ON COLUMN kb_bm25_v2_lifecycle.authority_content_revision IS
    'datasets.content_revision pinned for the current steady state (active_v2) or in-flight cutover; readiness compares against it';

CREATE INDEX IF NOT EXISTS idx_kb_bm25_v2_lifecycle_state
    ON kb_bm25_v2_lifecycle (state);

COMMIT;
