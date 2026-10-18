//! Canonical monitoring dashboard SQL.
pub const QUERY_0: &str = r####"
            SELECT
                (SELECT COUNT(*) FROM eval_datasets WHERE tenant_id = $1)::int AS dataset_count,
                (SELECT COUNT(*) FROM eval_examples WHERE tenant_id = $1)::int AS example_count,
                (SELECT COUNT(*) FROM eval_evaluators WHERE tenant_id = $1)::int AS evaluator_count,
                (SELECT COUNT(*) FROM eval_experiments WHERE tenant_id = $1)::int AS experiment_count,
                (SELECT COUNT(*) FROM eval_experiment_runs WHERE tenant_id = $1)::int AS run_count,
                (SELECT COUNT(*) FROM eval_examples
                 WHERE tenant_id = $1 AND metadata->>'review_status' = 'pending')::int AS judge_pending_count
            "####;
pub const QUERY_1: &str = r####"
            SELECT
                COUNT(*) FILTER (WHERE status = 'queued')::int AS queued_runs,
                COUNT(*) FILTER (WHERE status = 'running')::int AS running_runs,
                COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_runs,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_runs
            FROM eval_experiment_runs
            WHERE tenant_id = $1
            "####;
pub const QUERY_2: &str = r####"
            SELECT
                COUNT(*) FILTER (WHERE status = 'queued')::int AS queued_jobs,
                COUNT(*) FILTER (WHERE status = 'running')::int AS running_jobs,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_jobs
            FROM agent_trace_outbox
            WHERE tenant_id = $1
            "####;
pub const QUERY_3: &str = r####"
            SELECT
                COUNT(*) FILTER (WHERE trace_family = 'assistant')::int AS assistant_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND status = 'succeeded')::int AS assistant_succeeded_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND status = 'failed')::int AS assistant_failed_traces,
                COUNT(*) FILTER (WHERE trace_family = 'rag')::int AS rag_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'langgraph_proxy')::int AS langgraph_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND metadata ? 'runtime_trajectory')::int AS runtime_trajectory_traces,
                COUNT(*) FILTER (
                    WHERE trace_family = 'assistant'
                      AND COALESCE(metadata->'runtime_trajectory'->'trace_writer_health'->>'issue_count', '0')::int > 0
                )::int AS trace_writer_issue_traces,
                COUNT(*) FILTER (
                    WHERE trace_family = 'assistant'
                      AND COALESCE(metadata->'runtime_trajectory'->>'exit_reason', '') IN (
                          'tool_error',
                          'approval_denied',
                          'approval_required',
                          'max_iterations',
                          'interrupted',
                          'cancelled',
                          'timeout'
                      )
                )::int AS critical_runtime_failures
            FROM agent_traces
            WHERE tenant_id = $1
              AND created_at >= NOW() - make_interval(days => $2::int)
            "####;
pub const QUERY_4: &str = r####"
            SELECT COUNT(DISTINCT s.trace_id)::int AS tool_safety_failures
            FROM agent_trace_spans s
            INNER JOIN agent_traces t ON t.trace_id = s.trace_id
            WHERE t.tenant_id = $1
              AND t.created_at >= NOW() - make_interval(days => $2::int)
              AND (
                  s.attributes->>'direct_registry_denied' = 'true'
                  OR COALESCE(s.attributes->'gateway_policy_decision'->>'decision', '') IN ('deny', 'denied', 'blocked')
                  OR COALESCE(s.attributes->'sandbox_decision'->>'decision', '') IN ('deny', 'denied', 'blocked')
                  OR COALESCE(s.attributes->'sandbox_decision'->>'available', 'true') = 'false'
              )
            "####;
pub const QUERY_5: &str = r####"
            SELECT
                (
                    SELECT COALESCE(target_snapshot->>'candidate_label', run_id::text)
                    FROM eval_experiment_runs
                    WHERE tenant_id = $1
                      AND COALESCE(target_snapshot->>'candidate_label', '') ILIKE '%baseline%'
                    ORDER BY created_at DESC
                    LIMIT 1
                ) AS latest_baseline,
                (
                    SELECT COALESCE(target_snapshot->>'candidate_label', run_id::text)
                    FROM eval_experiment_runs
                    WHERE tenant_id = $1
                      AND (
                          COALESCE(target_snapshot->>'candidate_label', '') ILIKE '%candidate%'
                          OR COALESCE(target_snapshot->>'candidate_label', '') = ''
                      )
                    ORDER BY created_at DESC
                    LIMIT 1
                ) AS latest_candidate
            "####;
