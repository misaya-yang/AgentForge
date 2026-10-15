//! Durable runtime reconciliation SQL, copied from canonical source.
pub const CANDIDATES_0: &str = r####"SELECT r.run_id, r.tenant_id, r.user_id, r.session_id,
                      r.status, r.request_preview, r.usage, r.error, r.started_at,
                      COALESCE(r.finished_at, r.updated_at) AS ended_at,
                      s.runtime_thread_id, s.snapshot
               FROM assistant_runs r
               JOIN assistant_runtime_snapshots s
                 ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
                AND s.user_id = r.user_id AND s.session_id = r.session_id
                AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
               LEFT JOIN agent_traces t ON t.trace_id = r.run_id
               LEFT JOIN assistant.sessions a
                 ON a.session_id = r.session_id AND a.tenant_id = r.tenant_id
                AND a.user_id = r.user_id
               WHERE r.engine = 'agent_runtime'
                 AND r.status IN ('completed', 'succeeded', 'failed', 'cancelled')
                 AND COALESCE(r.finished_at, r.updated_at) >= $1::timestamptz
                 AND ($2::timestamptz IS NULL OR
                      (COALESCE(r.finished_at, r.updated_at), r.run_id) >
                      ($2::timestamptz, $3::uuid))
                 AND (t.trace_id IS NULL OR (
                     t.tenant_id = r.tenant_id AND t.user_id = r.user_id
                     AND t.session_id = r.session_id
                     AND t.workflow_kind = 'agent_runtime_turn'
                     AND (NOT EXISTS (
                         SELECT 1 FROM agent_trace_outbox o
                         WHERE o.tenant_id = r.tenant_id
                           AND o.job_type = 'trace.ingested'
                           AND o.payload->>'trace_id' = r.run_id::text
                     ) OR (
                         t.agent_id IS NULL AND a.agent_id IS NOT NULL
                         AND a.agent_id::text = s.snapshot->'agent_spec'->>'agentId'
                         AND a.agent_version_id::text IS NOT DISTINCT FROM
                             s.snapshot->'agent_spec'->>'agentVersionId'
                     ))
                 ))
               ORDER BY COALESCE(r.finished_at, r.updated_at), r.run_id
               LIMIT $4"####;
pub const TERMINAL_0: &str = r####"SELECT event_id, sequence, event_type, status, payload, created_at
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND event_type IN ('compat/v1/run_finished', 'compat/v1/run_error', 'compat/v1/cancelled')
                 AND payload->'data'->>'run_id' = $5
               ORDER BY sequence DESC LIMIT 1"####;
pub const COUNTS_0: &str = r####"SELECT event_type, COUNT(*)::int AS event_count
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND event_type LIKE 'compat/v1/%'
                 AND payload->'data'->>'run_id' = $5
               GROUP BY event_type"####;
pub const TEXT_0: &str = r####"SELECT sequence, payload->'data'->>'content' AS content
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND sequence > $6 AND event_type = 'compat/v1/text_delta'
                 AND payload->'data'->>'run_id' = $5
                 AND NULLIF(payload->'data'->>'content', '') IS NOT NULL
               ORDER BY sequence LIMIT $7"####;
pub const INSERT_0: &str = r####"SELECT r.run_id FROM assistant_runs r
                   JOIN assistant_runtime_snapshots s
                     ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
                    AND s.user_id = r.user_id AND s.session_id = r.session_id
                    AND s.runtime_thread_id = $5::uuid
                    AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
                   WHERE r.run_id = $1::uuid AND r.tenant_id = $2
                     AND r.user_id = $3 AND r.session_id = $4
                     AND r.engine = 'agent_runtime' AND r.status = $6
                     AND r.status IN ('completed', 'succeeded', 'failed', 'cancelled')
                     AND EXISTS (
                         SELECT 1 FROM assistant_runtime_items i
                         WHERE i.runtime_thread_id = s.runtime_thread_id
                           AND i.tenant_id = r.tenant_id AND i.user_id = r.user_id
                           AND i.session_id = r.session_id AND i.turn_id = r.run_id::text
                           AND i.event_id = $7::uuid
                           AND i.event_type IN ('compat/v1/run_finished', 'compat/v1/run_error', 'compat/v1/cancelled')
                           AND i.payload->'data'->>'run_id' = r.run_id::text
                     ) FOR UPDATE OF r"####;
pub const INSERT_1: &str = r####"INSERT INTO agent_traces (
                     trace_id, trace_family, workflow_kind, tenant_id, user_id,
                     session_id, thread_id, run_id, request_id, model_id, provider,
                     status, started_at, ended_at, total_latency_ms,
                     first_token_latency_ms, input_tokens, output_tokens, total_tokens,
                     input_preview, output_preview, redaction_state, metadata,
                     metrics, privacy, source_adapter, retention_expires_at
                   ) VALUES (
                     $1::uuid, 'assistant', 'agent_runtime_turn', $2, $3,
                     $4, $4, $5, $6, $7, $8,
                     $9, $10::timestamptz, $11::timestamptz, $12,
                     $13, $14, $15, $16,
                     $17, $18, $19::jsonb, $20::jsonb,
                     $21::jsonb, $22::jsonb, 'gateway.agent_runtime', $23::timestamptz
                   ) ON CONFLICT (trace_id) DO NOTHING RETURNING trace_id"####;
pub const INSERT_2: &str = r####"SELECT trace_id FROM agent_traces
                       WHERE trace_id = $1::uuid AND tenant_id = $2 AND user_id = $3
                         AND session_id = $4 AND run_id = $5
                         AND trace_family = 'assistant'
                         AND workflow_kind = 'agent_runtime_turn'
                         AND status = $6
                       FOR UPDATE"####;
pub const INSERT_3: &str = r####"INSERT INTO agent_trace_spans (
                     span_id, trace_id, span_kind, name, status, sequence_no,
                     started_at, ended_at, duration_ms, input_preview,
                     output_preview, attributes, error_type
                   ) VALUES ($1::uuid, $2::uuid, $3, $4, $5, $6,
                     $7::timestamptz, $8::timestamptz, $9, $10, $11,
                     $12::jsonb, $13) ON CONFLICT (span_id) DO NOTHING"####;
pub const INSERT_4: &str = r####"INSERT INTO agent_trace_events (
                     event_id, trace_id, event_type, sequence_no, occurred_at,
                     payload, payload_size_bytes, redacted
                   ) VALUES ($1::uuid, $2::uuid, $3, $4, $5::timestamptz,
                     $6::jsonb, 0, TRUE) ON CONFLICT (trace_id, sequence_no) DO NOTHING"####;
pub const INSERT_5: &str = r####"INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
                   SELECT $1::varchar, 'trace.ingested'::varchar, $2::jsonb
                   WHERE NOT EXISTS (
                       SELECT 1 FROM agent_trace_outbox
                       WHERE tenant_id = $1::varchar AND job_type = 'trace.ingested'
                         AND payload->>'trace_id' = $3::text
                   ) RETURNING job_id"####;

pub const USAGE_0: &str = r####"SELECT
                 COUNT(*) FILTER (WHERE c.dispatched_at IS NOT NULL)::int AS dispatched_calls,
                 COUNT(*) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                       AND c.input_tokens IS NOT NULL AND c.output_tokens IS NOT NULL
                 )::int AS measured_calls,
                 COALESCE(SUM(c.input_tokens) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS input_tokens,
                 COALESCE(SUM(c.output_tokens) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS output_tokens,
                 COALESCE(SUM(c.cost_microusd) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS cost_microusd,
                 COUNT(*) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                       AND c.cost_microusd IS NOT NULL
                 )::int AS cost_measured_calls
               FROM assistant_runtime_model_calls c
               JOIN assistant_runtime_snapshots s
                 ON s.run_id = c.run_id AND s.tenant_id = c.tenant_id
                AND s.user_id = c.user_id AND s.session_id = c.session_id
                AND s.runtime_thread_id = $4::uuid
               WHERE c.tenant_id = $1 AND c.user_id = $2 AND c.session_id = $3
                 AND c.run_id = $5::uuid"####;
