//! Canonical statement bodies from AgentTraceRepository.
pub const CREATE_SCORE: &str = r###"WITH saved AS (
            INSERT INTO agent_trace_scores (
                score_id, trace_id, span_id, score_name, score_type, numeric_value,
                boolean_value, categorical_value, text_value, label, explanation,
                scorer_type, evaluator_version, target_type, target_id,
                evaluator_id, evaluator_name, score_source, confidence,
                created_by, metadata
            ) VALUES (
                COALESCE($1::uuid, gen_random_uuid()), $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13, $14, $15,
                $16::uuid, $17, $18, $19,
                $20, $21::jsonb
            )
            ON CONFLICT (score_id)
            DO UPDATE SET
                score_type = EXCLUDED.score_type,
                numeric_value = EXCLUDED.numeric_value,
                boolean_value = EXCLUDED.boolean_value,
                categorical_value = EXCLUDED.categorical_value,
                text_value = EXCLUDED.text_value,
                label = EXCLUDED.label,
                explanation = EXCLUDED.explanation,
                scorer_type = EXCLUDED.scorer_type,
                evaluator_version = EXCLUDED.evaluator_version,
                target_type = EXCLUDED.target_type,
                target_id = EXCLUDED.target_id,
                evaluator_id = EXCLUDED.evaluator_id,
                evaluator_name = EXCLUDED.evaluator_name,
                score_source = EXCLUDED.score_source,
                confidence = EXCLUDED.confidence,
                created_by = EXCLUDED.created_by,
                metadata = EXCLUDED.metadata
            RETURNING *
            ) SELECT to_jsonb(saved) FROM saved"###;

pub const INGEST_TRACE: &str = r###"
            INSERT INTO agent_traces (
                trace_id, trace_family, workflow_kind, tenant_id, user_id,
                session_id, thread_id, run_id, request_id, otel_trace_id, traceparent,
                model_id, provider,
                status, started_at, ended_at, total_latency_ms, first_token_latency_ms,
                input_tokens, output_tokens, total_tokens, total_cost_cents,
                input_preview, output_preview, redaction_state, metadata,
                metrics, privacy, source_adapter, retention_expires_at
            ) VALUES (
                $1::uuid, $2, $3, $4, $5,
                $6, $7, $8, $9, $10, $11,
                $12, $13,
                $14, COALESCE($15::timestamptz, NOW()), $16::timestamptz, $17, $18,
                $19, $20, $21, $22,
                $23, $24, $25::jsonb, $26::jsonb,
                $27::jsonb, $28::jsonb, $29, $30::timestamptz
            )
            ON CONFLICT (trace_id)
            DO UPDATE SET
                trace_family = EXCLUDED.trace_family,
                workflow_kind = EXCLUDED.workflow_kind,
                user_id = EXCLUDED.user_id,
                session_id = EXCLUDED.session_id,
                thread_id = EXCLUDED.thread_id,
                run_id = EXCLUDED.run_id,
                request_id = EXCLUDED.request_id,
                model_id = EXCLUDED.model_id,
                provider = EXCLUDED.provider,
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_traces.ended_at),
                total_latency_ms = GREATEST(agent_traces.total_latency_ms, EXCLUDED.total_latency_ms),
                first_token_latency_ms = GREATEST(agent_traces.first_token_latency_ms, EXCLUDED.first_token_latency_ms),
                input_tokens = GREATEST(agent_traces.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(agent_traces.output_tokens, EXCLUDED.output_tokens),
                total_tokens = GREATEST(agent_traces.total_tokens, EXCLUDED.total_tokens),
                total_cost_cents = GREATEST(agent_traces.total_cost_cents, EXCLUDED.total_cost_cents),
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()
            WHERE agent_traces.tenant_id = EXCLUDED.tenant_id
              AND ($31::text IS NULL OR agent_traces.user_id = $31)
            RETURNING trace_id
            "###;

pub const UPSERT_SPAN: &str = r###"
            INSERT INTO agent_trace_spans (
                span_id, trace_id, parent_span_id, span_kind, name, status,
                sequence_no, started_at, ended_at, duration_ms, input_preview,
                output_preview, attributes, error_type, error_message
            ) VALUES (
                $1::uuid, $2::uuid, $3::uuid, $4, $5, $6,
                $7, COALESCE($8::timestamptz, NOW()), $9::timestamptz, $10, $11,
                $12, $13::jsonb, $14, $15
            )
            ON CONFLICT (span_id)
            DO UPDATE SET
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)
            WHERE agent_trace_spans.trace_id = EXCLUDED.trace_id
            RETURNING span_id
            "###;

pub const UPSERT_EVENT: &str = r###"
            INSERT INTO agent_trace_events (
                trace_id, span_id, event_type, sequence_no, occurred_at,
                payload, payload_size_bytes, redacted
            ) VALUES (
                $1::uuid, $2::uuid, $3, $4, COALESCE($5::timestamptz, NOW()),
                $6::jsonb, $7, $8
            )
            ON CONFLICT (trace_id, sequence_no)
            DO UPDATE SET
                span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted
            RETURNING event_id
            "###;

pub const BIND_RUNTIME_DIMENSIONS: &str = r###"UPDATE agent_traces t
           SET agent_id = a.agent_id, agent_version_id = a.agent_version_id,
               agent_draft_revision = a.agent_draft_revision,
               publication_id = a.publication_id, channel = a.channel,
               runtime_fingerprint = a.runtime_fingerprint,
               agent_spec_hash = a.agent_spec_hash
           FROM assistant_runs r
           JOIN assistant_runtime_snapshots s
             ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
            AND s.user_id = r.user_id AND s.session_id = r.session_id
            AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
           JOIN assistant.sessions a
             ON a.session_id = r.session_id AND a.tenant_id = r.tenant_id
            AND a.user_id = r.user_id
            AND a.agent_id::text = s.snapshot->'agent_spec'->>'agentId'
            AND a.agent_version_id::text IS NOT DISTINCT FROM
                s.snapshot->'agent_spec'->>'agentVersionId'
           WHERE t.trace_id = $1::uuid AND t.tenant_id = $2 AND t.user_id = $3
             AND t.session_id = $4 AND t.trace_family = 'assistant'
             AND t.run_id = r.run_id::text AND t.trace_id = r.run_id
             AND t.tenant_id = r.tenant_id AND t.user_id = r.user_id
             AND t.session_id = r.session_id AND r.engine = 'agent_runtime'
             AND t.agent_id IS NULL AND a.agent_id IS NOT NULL"###;

pub const ENQUEUE_TRACE: &str = r###"
WITH saved AS (
    INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
    SELECT $1::varchar, 'trace.ingested'::varchar, $2::jsonb
    WHERE NOT EXISTS (
        SELECT 1 FROM agent_trace_outbox
        WHERE tenant_id=$1::varchar AND job_type='trace.ingested'
          AND payload->>'trace_id'=$3::text
          AND ($4::boolean OR status IN ('queued', 'running'))
    ) RETURNING *
) SELECT to_jsonb(saved) FROM saved
"###;
