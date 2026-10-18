//! Canonical AgentTraceRepository aggregate SQL. Only filter placeholders are substituted.
pub const OPS_0: &str = r####"
            WITH filtered_traces AS (
                SELECT
                    trace_id, tenant_id, session_id, agent_version_id,
                    publication_id, channel, status, total_latency_ms,
                    first_token_latency_ms, total_tokens, total_cost_cents,
                    created_at
                FROM agent_traces
                WHERE {{where_clause}}
            ),
            trace_metrics AS (
                SELECT
                    COUNT(*)::int AS total_runs,
                    COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_runs,
                    COUNT(*) FILTER (WHERE status IN ('failed', 'timeout'))::int AS failed_runs,
                    COUNT(DISTINCT session_id)::int AS sessions,
                    COALESCE(AVG(total_latency_ms), 0)::int AS avg_latency_ms,
                    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p50_latency_ms,
                    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                    COALESCE(AVG(first_token_latency_ms), 0)::int AS avg_ttft_ms,
                    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY first_token_latency_ms), 0)::int AS p50_ttft_ms,
                    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY first_token_latency_ms), 0)::int AS p95_ttft_ms,
                    COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                    COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents,
                    MIN(created_at) AS oldest_trace_at,
                    MAX(created_at) AS newest_trace_at
                FROM filtered_traces
            ),
            classified_spans AS (
                SELECT
                    s.span_kind,
                    s.status,
                    CASE
                        WHEN COALESCE(
                            s.attributes->>'retrieval.document_count',
                            s.attributes#>>'{retrieval,document_count}'
                        ) ~ '^[0-9]+$'
                        THEN COALESCE(
                            s.attributes->>'retrieval.document_count',
                            s.attributes#>>'{retrieval,document_count}'
                        )::int
                        WHEN jsonb_typeof(COALESCE(
                            s.attributes->'retrieval.documents',
                            s.attributes#>'{retrieval,documents}'
                        )) = 'array'
                        THEN jsonb_array_length(COALESCE(
                            s.attributes->'retrieval.documents',
                            s.attributes#>'{retrieval,documents}'
                        ))
                        ELSE 0
                    END AS retrieved_document_count
                FROM agent_trace_spans s
                INNER JOIN filtered_traces t ON t.trace_id = s.trace_id
                WHERE s.span_kind IN ('tool_execution', 'retriever')
            ),
            span_metrics AS (
                SELECT
                    COUNT(*) FILTER (WHERE span_kind = 'tool_execution')::int AS tool_calls,
                    COUNT(*) FILTER (
                        WHERE span_kind = 'tool_execution' AND status = 'succeeded'
                    )::int AS tool_succeeded,
                    COUNT(*) FILTER (WHERE span_kind = 'retriever')::int AS knowledge_queries,
                    COUNT(*) FILTER (
                        WHERE span_kind = 'retriever'
                          AND status = 'succeeded'
                          AND retrieved_document_count > 0
                    )::int AS knowledge_hits
                FROM classified_spans
            ),
            feedback_metrics AS (
                SELECT
                    COUNT(*)::int AS feedback_count,
                    COUNT(*) FILTER (WHERE f.rating = 1)::int AS positive_feedback_count
                FROM agent_runtime_feedback f
                WHERE f.tenant_id = $1
                  AND EXISTS (
                      SELECT 1
                      FROM filtered_traces t
                      WHERE t.tenant_id = f.tenant_id
                        AND t.agent_version_id = f.agent_version_id
                        AND t.publication_id = f.publication_id
                        AND t.channel = f.channel
                        AND t.session_id = f.session_id
                  )
            )
            SELECT trace_metrics.*, span_metrics.*, feedback_metrics.*
            FROM trace_metrics
            CROSS JOIN span_metrics
            CROSS JOIN feedback_metrics
            "####;
pub const OPS_1: &str = r####"
            SELECT
                channel,
                agent_version_id,
                publication_id,
                COUNT(*)::int AS run_count,
                COUNT(*) FILTER (WHERE status IN ('failed', 'timeout'))::int AS failed_count,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents
            FROM agent_traces
            WHERE {{where_clause}}
            GROUP BY channel, agent_version_id, publication_id
            ORDER BY run_count DESC, channel ASC
            "####;
pub const OPS_2: &str = r####"
            SELECT
                COALESCE(p.trace_retention_days, 90)::int AS trace_retention_days,
                COALESCE(p.legal_hold, FALSE) AS legal_hold,
                (
                    SELECT MAX(r.completed_at)
                    FROM agent_data_deletion_requests r
                    WHERE r.tenant_id = $1 AND r.agent_id = $2::uuid
                      AND r.scope = 'retention' AND r.status = 'completed'
                ) AS last_retention_cleanup_at
            FROM agents a
            LEFT JOIN agent_governance_policies p
              ON p.tenant_id = a.tenant_id AND p.agent_id = a.agent_id
            WHERE a.tenant_id = $1 AND a.agent_id = $2::uuid
            "####;
pub const SUMMARY_0: &str = r####"
            SELECT
                COUNT(*)::int AS total_traces,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_traces,
                COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant')::int AS assistant_traces,
                COUNT(*) FILTER (WHERE trace_family = 'langgraph_proxy')::int AS langgraph_traces,
                COUNT(*) FILTER (WHERE trace_family = 'rag')::int AS rag_traces,
                COALESCE(AVG(total_latency_ms), 0)::int AS avg_latency_ms,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents
            FROM agent_traces
            WHERE {{where_clause}}
            "####;
pub const SUMMARY_1: &str = r####"
            SELECT COUNT(DISTINCT s.trace_id)::int AS scored_traces
            FROM agent_trace_scores s
            INNER JOIN agent_traces t ON t.trace_id = s.trace_id
            WHERE {{where_clause}}
            "####;
pub const RAGAS_0: &str = r####"
            WITH {{latest_scores_ctes}}
            SELECT
                (
                    SELECT COUNT(DISTINCT t.trace_id)
                    FROM in_window_traces t
                )::int AS rag_traces,
                COUNT(DISTINCT CASE
                    WHEN s.label IN ('pass', 'fail') THEN s.trace_id
                END)::int AS ragas_scored_traces
            FROM latest_scores s
            "####;
pub const RAGAS_1: &str = r####"
            WITH {{latest_scores_ctes}}
            SELECT
                s.score_name AS metric,
                COALESCE(
                    AVG(s.numeric_value) FILTER (WHERE s.label IN ('pass', 'fail')),
                    0
                )::float AS average_score,
                COUNT(*) FILTER (WHERE s.label IN ('pass', 'fail'))::int AS scored_count,
                COUNT(*) FILTER (WHERE s.label = 'pass')::int AS pass_count,
                COUNT(*) FILTER (WHERE s.label = 'fail')::int AS fail_count,
                COUNT(*) FILTER (WHERE s.label = 'review')::int AS review_count
            FROM latest_scores s
            GROUP BY s.score_name
            ORDER BY s.score_name
            "####;
pub const RAGAS_2: &str = r####"
            WITH {{latest_scores_ctes}}
            SELECT s.metadata->>'judge_model' AS judge_model
            FROM latest_scores s
            WHERE COALESCE(s.metadata->>'judge_model', '') <> ''
            ORDER BY s.created_at DESC, s.score_id DESC
            LIMIT 1
            "####;
pub const RAGAS_CTES: &str = r####"
            in_window_traces AS (
                SELECT t.trace_id
                FROM agent_traces t
                WHERE {{where_clause}}
            ),
            latest_scores AS (
                SELECT ranked_scores.*
                FROM (
                    SELECT
                        s.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY s.trace_id, s.evaluator_id,
                                COALESCE(s.evaluator_version, ''), s.score_name
                            ORDER BY s.created_at DESC, s.score_id DESC
                        ) AS score_revision
                    FROM agent_trace_scores s
                    INNER JOIN in_window_traces t ON t.trace_id = s.trace_id
                    WHERE s.score_source = 'kb_ragas'
                ) ranked_scores
                WHERE ranked_scores.score_revision = 1
            )
        "####;
