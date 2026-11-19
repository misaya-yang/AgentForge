-- Migration: 062_agent_trace_search_indexes.sql
-- Additive search indexes for Eval trace list transcript/metadata filters.

BEGIN;

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_agent_traces_tenant_family_created
    ON agent_traces(tenant_id, trace_family, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_agent_traces_input_preview_trgm
    ON agent_traces USING gin (input_preview gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_agent_traces_output_preview_trgm
    ON agent_traces USING gin (output_preview gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_agent_traces_metadata_transcript_trgm
    ON agent_traces USING gin (
        (
            COALESCE(metadata->'transcript_locator'->>'current_message_preview', '')
            || ' '
            || COALESCE(metadata->'transcript_locator'->>'transcript_excerpt', '')
            || ' '
            || COALESCE(metadata->'transcript_locator'->>'turn_id', '')
        ) gin_trgm_ops
    );

CREATE INDEX IF NOT EXISTS idx_agent_trace_spans_trace_kind
    ON agent_trace_spans(trace_id, span_kind);

COMMIT;