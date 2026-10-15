-- KB golden QA version store (PRD T0-#2: 双语黄金集, 版本钉进 Postgres,
-- 分冻结回归集与增长集).
--
-- Central-ledger migration (root database/migrations chain). Additive only,
-- idempotent (IF NOT EXISTS), and self-contained: no foreign keys, so the
-- golden ledger never couples to dataset/document lifecycles — deleting a KB
-- dataset must never touch evaluation ground truth.
--
-- The git-pinned JSONL under tests/fixtures/eval/rag/golden/ stays the
-- reviewed source of truth (manifest-hashed by make kb-golden-gate); this
-- table is the version-pinned queryable projection that gates, baselines and
-- admin surfaces read. Rows are (version, case_id) snapshots imported by
-- scripts/import_kb_eval_golden.py, whose shape contract is the same one
-- validate_rag_cases enforces (track/query/relevance/reference_answer/
-- metadata). ``split`` separates the frozen regression set (rows a frozen
-- baseline was computed on; promotion is an explicit, review-gated write)
-- from the growth set (fresh additions awaiting review).
-- kb_eval_golden_release is the pointer table pinning which version the
-- current gates cite, one row per release key.

BEGIN;

CREATE SCHEMA IF NOT EXISTS knowledge;
SET LOCAL search_path = knowledge, gateway, assistant, public;

CREATE TABLE IF NOT EXISTS kb_eval_golden (
    case_id          VARCHAR(255) NOT NULL,
    version          VARCHAR(100) NOT NULL,
    track            VARCHAR(20)  NOT NULL CHECK (track IN ('retrieval_only', 'answer_aware')),
    query            TEXT         NOT NULL CHECK (length(btrim(query)) > 0),
    relevance        JSONB        NOT NULL CHECK (jsonb_typeof(relevance) = 'object'),
    reference_answer TEXT,
    split            VARCHAR(20)  NOT NULL DEFAULT 'growth' CHECK (split IN ('frozen', 'growth')),
    metadata         JSONB        NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    provenance       VARCHAR(100) NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (version, case_id)
);

COMMENT ON TABLE kb_eval_golden IS
    'PRD T0-#2: version-pinned bilingual KB golden QA rows (projection of the manifest-pinned JSONL)';
COMMENT ON COLUMN kb_eval_golden.split IS
    'frozen = regression rows a frozen baseline was computed on (explicit review-gated promotion); growth = additions pending review';

CREATE INDEX IF NOT EXISTS idx_kb_eval_golden_split
    ON kb_eval_golden (version, split);

CREATE TABLE IF NOT EXISTS kb_eval_golden_release (
    release_key VARCHAR(50) PRIMARY KEY,
    version     VARCHAR(100) NOT NULL,
    note        TEXT         NOT NULL DEFAULT '',
    set_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE kb_eval_golden_release IS
    'PRD T0-#2: pointer pinning which kb_eval_golden version the gates/baselines evaluate against';

COMMIT;
