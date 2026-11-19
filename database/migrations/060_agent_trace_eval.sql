-- Migration: 060_agent_trace_eval.sql
-- Goal:
-- 1) Add tenant-scoped Agent Trace Eval tables for AI Assistant first wave
-- 2) Reserve trace_family values for later LangGraph proxy and RAG trace families
-- 3) Store redacted previews, metrics, spans, events, and evaluator scores

BEGIN;

-- -------------------------------------------------------------------------
-- agent_traces: root trace record for AI Assistant and future trace families
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS agent_traces (
    trace_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trace_family VARCHAR(32) NOT NULL DEFAULT 'assistant',
    workflow_kind VARCHAR(64) NOT NULL DEFAULT 'ai_assistant_chat',
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100),
    run_id VARCHAR(100),
    request_id VARCHAR(100),
    otel_trace_id VARCHAR(128),
    traceparent VARCHAR(255),
    model_id VARCHAR(128),
    provider VARCHAR(64),
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    first_token_latency_ms INTEGER NOT NULL DEFAULT 0,
    total_latency_ms INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    total_cost_cents INTEGER NOT NULL DEFAULT 0,
    input_preview TEXT NOT NULL DEFAULT '',
    output_preview TEXT NOT NULL DEFAULT '',
    redaction_state JSONB NOT NULL DEFAULT '{}'::jsonb,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    retention_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_traces_trace_family'
    ) THEN
        ALTER TABLE agent_traces
        ADD CONSTRAINT chk_agent_traces_trace_family
        CHECK (trace_family IN ('assistant', 'langgraph_proxy', 'rag'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_traces_status'
    ) THEN
        ALTER TABLE agent_traces
        ADD CONSTRAINT chk_agent_traces_status
        CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'timeout'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_created
    ON agent_traces(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_family_created
    ON agent_traces(tenant_id, trace_family, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_user_created
    ON agent_traces(tenant_id, user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_session
    ON agent_traces(tenant_id, session_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_run
    ON agent_traces(tenant_id, run_id);
CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_request
    ON agent_traces(tenant_id, request_id);
CREATE INDEX IF NOT EXISTS idx_agent_traces_status_created
    ON agent_traces(status, created_at DESC);

-- -------------------------------------------------------------------------
-- agent_trace_spans: nested execution steps
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS agent_trace_spans (
    span_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trace_id UUID NOT NULL REFERENCES agent_traces(trace_id) ON DELETE CASCADE,
    parent_span_id UUID REFERENCES agent_trace_spans(span_id) ON DELETE SET NULL,
    span_kind VARCHAR(64) NOT NULL,
    name VARCHAR(160) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    sequence_no INTEGER NOT NULL DEFAULT 0,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    input_preview TEXT NOT NULL DEFAULT '',
    output_preview TEXT NOT NULL DEFAULT '',
    attributes JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_type VARCHAR(64),
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_spans_status'
    ) THEN
        ALTER TABLE agent_trace_spans
        ADD CONSTRAINT chk_agent_trace_spans_status
        CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'skipped'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_trace_spans_trace_sequence
    ON agent_trace_spans(trace_id, sequence_no, started_at);
CREATE INDEX IF NOT EXISTS idx_agent_trace_spans_kind
    ON agent_trace_spans(span_kind, started_at DESC);

-- -------------------------------------------------------------------------
-- agent_trace_events: ordered lifecycle and stream events
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS agent_trace_events (
    event_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trace_id UUID NOT NULL REFERENCES agent_traces(trace_id) ON DELETE CASCADE,
    span_id UUID REFERENCES agent_trace_spans(span_id) ON DELETE SET NULL,
    event_type VARCHAR(96) NOT NULL,
    sequence_no INTEGER NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    payload_size_bytes INTEGER NOT NULL DEFAULT 0,
    redacted BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_trace_events_trace_sequence_unique
    ON agent_trace_events(trace_id, sequence_no);
CREATE INDEX IF NOT EXISTS idx_agent_trace_events_trace_created
    ON agent_trace_events(trace_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_trace_events_type
    ON agent_trace_events(event_type, occurred_at DESC);

-- -------------------------------------------------------------------------
-- agent_trace_scores: manual or evaluator feedback
-- -------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS agent_trace_scores (
    score_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trace_id UUID NOT NULL REFERENCES agent_traces(trace_id) ON DELETE CASCADE,
    span_id UUID REFERENCES agent_trace_spans(span_id) ON DELETE SET NULL,
    score_name VARCHAR(96) NOT NULL,
    score_type VARCHAR(32) NOT NULL DEFAULT 'numeric',
    numeric_value DOUBLE PRECISION,
    boolean_value BOOLEAN,
    categorical_value VARCHAR(96),
    text_value TEXT,
    label VARCHAR(96),
    explanation TEXT,
    scorer_type VARCHAR(32) NOT NULL DEFAULT 'human',
    evaluator_version VARCHAR(64),
    created_by VARCHAR(64) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_scores_type'
    ) THEN
        ALTER TABLE agent_trace_scores
        ADD CONSTRAINT chk_agent_trace_scores_type
        CHECK (score_type IN ('numeric', 'categorical', 'boolean', 'text'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_agent_trace_scores_scorer'
    ) THEN
        ALTER TABLE agent_trace_scores
        ADD CONSTRAINT chk_agent_trace_scores_scorer
        CHECK (scorer_type IN ('human', 'llm', 'rule', 'system'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_agent_trace_scores_trace_created
    ON agent_trace_scores(trace_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_trace_scores_created_by
    ON agent_trace_scores(created_by, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_trace_scores_name
    ON agent_trace_scores(score_name, created_at DESC);

-- -------------------------------------------------------------------------
-- updated_at trigger for root traces
-- -------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION update_agent_traces_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_agent_traces_timestamp ON agent_traces;
CREATE TRIGGER update_agent_traces_timestamp
    BEFORE UPDATE ON agent_traces
    FOR EACH ROW
    EXECUTE FUNCTION update_agent_traces_timestamp();

COMMIT;
