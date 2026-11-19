-- INERT, unnumbered authority successor; install after write-compatibility.sql.
-- Exact native conflict contracts. Do not grant writer roles policy-table access.
-- Any changed/new statement fails closed until its policy is re-admitted.
SET LOCAL lock_timeout='2s';SET LOCAL statement_timeout='60s';
-- platform-gateway-foundation/src/usage_recorder.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.usage_records','ON CONFLICT (tenant_id,(COALESCE(request_id,'''')),(COALESCE(service_id,'''')),(COALESCE(request_type,''''))) WHERE request_id IS NOT NULL AND request_id<>'''' DO UPDATE SET input_tokens=GREATEST(usage_records.input_tokens,EXCLUDED.input_tokens),output_tokens=GREATEST(usage_records.output_tokens,EXCLUDED.output_tokens),input_cost_cents=GREATEST(usage_records.input_cost_cents,EXCLUDED.input_cost_cents),output_cost_cents=GREATEST(usage_records.output_cost_cents,EXCLUDED.output_cost_cents),status=CASE WHEN usage_records.status IN (''running'',''pending'') AND EXCLUDED.status=''success'' THEN EXCLUDED.status ELSE usage_records.status END,error_type=COALESCE(usage_records.error_type,EXCLUDED.error_type),metadata=COALESCE(usage_records.metadata,''{}''::jsonb)||COALESCE(EXCLUDED.metadata,''{}''::jsonb)||CASE WHEN usage_records.metadata->''pricing_snapshot'' IS NOT NULL AND usage_records.metadata->''pricing_snapshot''<>''null''::jsonb THEN jsonb_build_object(''pricing_snapshot'',usage_records.metadata->''pricing_snapshot'') ELSE ''{}''::jsonb END WHERE (COALESCE(usage_records.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''' AND COALESCE(EXCLUDED.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''') OR (usage_records.provider IS NOT DISTINCT FROM EXCLUDED.provider AND usage_records.model=EXCLUDED.model AND usage_records.metadata->''pricing_snapshot''=EXCLUDED.metadata->''pricing_snapshot'') RETURNING (xmax=0) AS inserted,tenant_id,COALESCE(request_id,'''') AS request_id_key,COALESCE(service_id,'''') AS service_id_key,COALESCE(request_type,'''') AS request_type_key',NULL,'idx_usage_records_request_identity','update','input_tokens=GREATEST(usage_records.input_tokens,EXCLUDED.input_tokens),output_tokens=GREATEST(usage_records.output_tokens,EXCLUDED.output_tokens),input_cost_cents=GREATEST(usage_records.input_cost_cents,EXCLUDED.input_cost_cents),output_cost_cents=GREATEST(usage_records.output_cost_cents,EXCLUDED.output_cost_cents),status=CASE WHEN usage_records.status IN (''running'',''pending'') AND EXCLUDED.status=''success'' THEN EXCLUDED.status ELSE usage_records.status END,error_type=COALESCE(usage_records.error_type,EXCLUDED.error_type),metadata=COALESCE(usage_records.metadata,''{}''::jsonb)||COALESCE(EXCLUDED.metadata,''{}''::jsonb)||CASE WHEN usage_records.metadata->''pricing_snapshot'' IS NOT NULL AND usage_records.metadata->''pricing_snapshot''<>''null''::jsonb THEN jsonb_build_object(''pricing_snapshot'',usage_records.metadata->''pricing_snapshot'') ELSE ''{}''::jsonb END','(COALESCE(usage_records.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''' AND COALESCE(EXCLUDED.metadata->''pricing_snapshot''->>''price_book_schema'','''')='''') OR (usage_records.provider IS NOT DISTINCT FROM EXCLUDED.provider AND usage_records.model=EXCLUDED.model AND usage_records.metadata->''pricing_snapshot''=EXCLUDED.metadata->''pricing_snapshot'')');
-- platform-gateway-foundation/src/usage_recorder.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.billing_events','ON CONFLICT(event_id) DO NOTHING',ARRAY['event_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/usage_stage_receipts.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.billing_events','ON CONFLICT(event_id) DO NOTHING RETURNING event_id',ARRAY['event_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/usage_stage_receipts.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.price_book_v2_accounting_checkpoints','ON CONFLICT(tenant_id,request_id,service_id,request_type,stage) DO UPDATE SET observation=EXCLUDED.observation,updated_at=CURRENT_TIMESTAMP',ARRAY['tenant_id','request_id','service_id','request_type','stage']::name[],NULL,'update','observation=EXCLUDED.observation,updated_at=CURRENT_TIMESTAMP',NULL);
-- platform-gateway-foundation/src/provider_batch_settlement.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.usage_records','ON CONFLICT DO NOTHING',NULL,NULL,'nothing',NULL,NULL);
-- platform-gateway-foundation/src/provider_batch_settlement.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.price_book_v2_accounting_checkpoints','ON CONFLICT DO NOTHING',NULL,NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_traces','ON CONFLICT (trace_id)
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
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()
            WHERE agent_traces.tenant_id = EXCLUDED.tenant_id
              AND ($31::text IS NULL OR agent_traces.user_id = $31)
            RETURNING trace_id',ARRAY['trace_id']::name[],NULL,'update','trace_family = EXCLUDED.trace_family,
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
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()','agent_traces.tenant_id = EXCLUDED.tenant_id
              AND (__AF_P31__::text IS NULL OR agent_traces.user_id = __AF_P31__)');
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_spans','ON CONFLICT (span_id)
            DO UPDATE SET
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)
            WHERE agent_trace_spans.trace_id = EXCLUDED.trace_id
            RETURNING span_id',ARRAY['span_id']::name[],NULL,'update','status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)','agent_trace_spans.trace_id = EXCLUDED.trace_id');
-- platform-gateway-eval/src/migration_trace/sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_events','ON CONFLICT (trace_id, sequence_no)
            DO UPDATE SET
                span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted
            RETURNING event_id',ARRAY['trace_id','sequence_no']::name[],NULL,'update','span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted',NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_traces','ON CONFLICT (trace_id) DO NOTHING RETURNING trace_id',ARRAY['trace_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_spans','ON CONFLICT (span_id) DO NOTHING',ARRAY['span_id']::name[],NULL,'nothing',NULL,NULL);
-- platform-gateway-eval/src/migration_trace/reconcile_sql.rs
INSERT INTO partition_ops.statement_policies(family,statement_tail,target_columns,conflict_index,action,update_sql,where_sql) VALUES('gateway.agent_trace_events','ON CONFLICT (trace_id, sequence_no) DO NOTHING',ARRAY['trace_id','sequence_no']::name[],NULL,'nothing',NULL,NULL);
