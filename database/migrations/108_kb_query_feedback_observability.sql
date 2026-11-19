-- H1 #16/#25/#26: durable KB query observations, segment hit counters,
-- tenant-scoped query history, and human feedback.  The retrieval hot path
-- generates trace_id/query_fingerprint before returning; persistence remains
-- asynchronous and idempotent on trace_id.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

-- dataset_queries predates the split knowledge schema.  Resolve the canonical
-- relation through search_path so this migration works for public, main-N-1,
-- and already-split layouts without creating a duplicate table.
ALTER TABLE dataset_queries
    ADD COLUMN IF NOT EXISTS trace_id UUID,
    ADD COLUMN IF NOT EXISTS query_fingerprint VARCHAR(64),
    ADD COLUMN IF NOT EXISTS mode VARCHAR(32),
    ADD COLUMN IF NOT EXISTS top_k INTEGER,
    ADD COLUMN IF NOT EXISTS hit_count INTEGER,
    ADD COLUMN IF NOT EXISTS stage_timings JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Preserve useful migration-100 observations.  Legacy rows have no trace_id
-- and remain explicitly legacy; only validated values are projected.
UPDATE dataset_queries
SET query_fingerprint = metadata->>'query_fingerprint'
WHERE query_fingerprint IS NULL
  AND metadata->>'query_fingerprint' ~ '^[0-9a-f]{64}$';

UPDATE dataset_queries
SET mode = CASE
        WHEN metadata->>'mode' IN ('dense', 'bm25', 'hybrid', 'keyword', 'vector')
        THEN metadata->>'mode'
        ELSE mode
    END,
    top_k = CASE
        WHEN metadata->>'top_k' ~ '^[0-9]+$' THEN CASE
            WHEN (metadata->>'top_k')::numeric BETWEEN 1 AND 100
            THEN (metadata->>'top_k')::INTEGER
            ELSE top_k
        END
        ELSE top_k
    END,
    hit_count = CASE
        WHEN metadata->>'hit_count' ~ '^[0-9]+$' THEN CASE
            WHEN (metadata->>'hit_count')::numeric BETWEEN 0 AND 2147483647
            THEN (metadata->>'hit_count')::INTEGER
            ELSE hit_count
        END
        ELSE hit_count
    END,
    stage_timings = CASE
        WHEN jsonb_typeof(metadata->'stage_timings') = 'object'
        THEN metadata->'stage_timings'
        ELSE stage_timings
    END
WHERE (mode IS NULL AND metadata->>'mode' IN
        ('dense', 'bm25', 'hybrid', 'keyword', 'vector'))
   OR (top_k IS NULL
       AND CASE WHEN metadata->>'top_k' ~ '^[0-9]+$'
           THEN (metadata->>'top_k')::numeric BETWEEN 1 AND 100
           ELSE FALSE END)
   OR (hit_count IS NULL
       AND CASE WHEN metadata->>'hit_count' ~ '^[0-9]+$'
           THEN (metadata->>'hit_count')::numeric BETWEEN 0 AND 2147483647
           ELSE FALSE END)
   OR (stage_timings = '{}'::jsonb
       AND jsonb_typeof(metadata->'stage_timings') = 'object'
       AND metadata->'stage_timings' <> '{}'::jsonb);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'dataset_queries'::regclass
          AND conname = 'chk_dataset_queries_observation_shape'
    ) THEN
        ALTER TABLE dataset_queries
            ADD CONSTRAINT chk_dataset_queries_observation_shape CHECK (
                trace_id IS NULL OR (
                    query_fingerprint ~ '^[0-9a-f]{64}$'
                    AND mode IN ('dense', 'bm25', 'hybrid', 'keyword', 'vector')
                    AND top_k BETWEEN 1 AND 100
                    AND hit_count >= 0
                    AND jsonb_typeof(stage_timings) = 'object'
                )
            );
    END IF;
END
$$;

CREATE UNIQUE INDEX IF NOT EXISTS uq_dataset_queries_trace_id
    ON dataset_queries (trace_id)
    WHERE trace_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_dataset_queries_dataset_created_page
    ON dataset_queries (dataset_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_dataset_queries_dataset_zero_created
    ON dataset_queries (dataset_id, created_at DESC, id DESC)
    WHERE hit_count = 0;

-- Feedback deliberately has no FK to dataset_queries: query telemetry is
-- asynchronous/best-effort, while explicit user feedback is durable and must
-- not race the observation insert.  The immutable trace/fingerprint pair plus
-- tenant/dataset FK provides the correlation and isolation contract.
CREATE TABLE IF NOT EXISTS knowledge.dataset_query_feedback (
    feedback_id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id         VARCHAR(255) NOT NULL,
    dataset_id        VARCHAR(255) NOT NULL,
    trace_id          UUID NOT NULL,
    query_fingerprint VARCHAR(64) NOT NULL
        CHECK (query_fingerprint ~ '^[0-9a-f]{64}$'),
    target_type       VARCHAR(32) NOT NULL
        CHECK (target_type IN ('retrieval_hit', 'qa_answer')),
    target_id         VARCHAR(255) NOT NULL CHECK (length(btrim(target_id)) > 0),
    rating            VARCHAR(16) NOT NULL
        CHECK (rating IN ('positive', 'negative')),
    reason_code       VARCHAR(32) NOT NULL,
    comment           TEXT CHECK (comment IS NULL OR length(comment) <= 2000),
    created_by        VARCHAR(255) NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_dataset_query_feedback_dataset_tenant
        FOREIGN KEY (dataset_id, tenant_id)
        REFERENCES datasets (dataset_id, tenant_id) ON DELETE CASCADE,
    CONSTRAINT chk_dataset_query_feedback_reason CHECK (
        (rating = 'positive' AND reason_code IN
            ('relevant', 'helpful', 'well_cited', 'other'))
        OR
        (rating = 'negative' AND reason_code IN
            ('irrelevant', 'incorrect', 'missing_context',
             'bad_citation', 'stale', 'unsafe', 'other'))
    ),
    CONSTRAINT chk_dataset_query_feedback_target CHECK (
        (target_type = 'qa_answer' AND target_id = trace_id::text)
        OR
        (target_type = 'retrieval_hit' AND target_id <> trace_id::text)
    ),
    UNIQUE (tenant_id, dataset_id, trace_id, target_type, target_id, created_by)
);

CREATE INDEX IF NOT EXISTS idx_dataset_query_feedback_scope_page
    ON knowledge.dataset_query_feedback
        (tenant_id, dataset_id, created_at DESC, feedback_id DESC);
CREATE INDEX IF NOT EXISTS idx_dataset_query_feedback_negative_page
    ON knowledge.dataset_query_feedback
        (tenant_id, dataset_id, created_at DESC, feedback_id DESC)
    WHERE rating = 'negative';

ALTER TABLE dataset_queries OWNER TO CURRENT_USER;
ALTER TABLE knowledge.dataset_query_feedback OWNER TO CURRENT_USER;

COMMENT ON TABLE knowledge.dataset_query_feedback IS
    'H1 #25: durable tenant-scoped thumbs/reason feedback correlated by backend-generated KB trace';
COMMENT ON COLUMN dataset_queries.query_fingerprint IS
    'SHA-256 of NFKC + whitespace-normalized + case-folded query text; distinct from cache fingerprint';

COMMIT;
