use super::*;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
pub(super) fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::Number(n) => {
            let seconds = n.as_f64()?;
            if !seconds.is_finite() || seconds <= 0. {
                return None;
            }
            DateTime::from_timestamp(
                seconds.floor() as i64,
                ((seconds - seconds.floor()) * 1e9) as u32,
            )
        }
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|v| v.with_timezone(&Utc))
                .or_else(|| {
                    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
                        .iter()
                        .find_map(|f| {
                            NaiveDateTime::parse_from_str(s, f)
                                .ok()
                                .map(|v| v.and_utc())
                        })
                })
                .or_else(|| {
                    NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .ok()
                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                        .map(|v| v.and_utc())
                })
        }
        _ => None,
    }
}
fn integer(v: &Value) -> Result<i64, TraceError> {
    if !truthy(v) {
        return Ok(0);
    }
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite() && *f >= i64::MIN as f64 && *f < i64::MAX as f64)
                    .map(|f| f as i64)
            })
            .ok_or_else(TraceError::invalid),
        Value::String(s) => s.trim().parse().map_err(|_| TraceError::invalid()),
        _ => Err(TraceError::invalid()),
    }
}
fn default_object(v: &Value, key: &str) -> Value {
    v.get(key)
        .filter(|v| truthy(v))
        .cloned()
        .unwrap_or(json!({}))
}
impl TraceRepository {
    /// Trusted internal capture keeps its explicit subject attribution. Every
    /// write still has a tenant fence; public ingress adds the caller's owner scope.
    pub async fn ingest_trace(
        &self,
        tenant_id: &str,
        created_by: &str,
        payload: &Value,
        enqueue: bool,
    ) -> Result<Value, TraceError> {
        self.ingest_trace_scoped(tenant_id, created_by, payload, enqueue, None)
            .await
    }

    pub async fn ingest_trace_scoped(
        &self,
        tenant_id: &str,
        created_by: &str,
        payload: &Value,
        enqueue: bool,
        scope_user_id: Option<&str>,
    ) -> Result<Value, TraceError> {
        if self
            .claim
            .as_ref()
            .is_some_and(|claim| claim.tenant_id != tenant_id)
        {
            return Err(TraceError::lease_lost());
        }
        let trace = payload
            .get("trace")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(TraceError::invalid)?;
        let owner = string_or(&trace, "user_id", created_by);
        if tenant_id.is_empty() || scope_user_id.is_some_and(|id| id.is_empty() || id != owner) {
            return Err(TraceError::not_found());
        }
        let id = string_or(&trace, "trace_id", &uuid::Uuid::new_v4().to_string());
        let thread = trace
            .get("thread_id")
            .filter(|v| truthy(v))
            .or_else(|| trace.get("session_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let metrics = default_object(&trace, "metrics");
        let privacy = default_object(&trace, "privacy");
        let mut metadata = json!({"schema_version":"ate-03","source_adapter":string_or(&trace,"source_adapter","api")});
        if let Some(extra) = trace.get("metadata").and_then(Value::as_object) {
            metadata.as_object_mut().unwrap().extend(extra.clone());
        }
        let mut numbers = Vec::new();
        for key in [
            "total_latency_ms",
            "first_token_latency_ms",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "total_cost_cents",
        ] {
            numbers.push(integer(
                metrics
                    .get(key)
                    .filter(|v| truthy(v))
                    .or_else(|| trace.get(key))
                    .unwrap_or(&Value::Null),
            )?);
        }
        let record = trace.clone();
        let trace_query = sqlx::query(sql::INGEST_TRACE)
            .bind(id.clone())
            .bind(string_or(&record, "trace_family", "assistant"))
            .bind(string_or(&record, "workflow_kind", "ai_assistant_chat"))
            .bind(tenant_id.to_owned())
            .bind(owner.clone())
            .bind(record["session_id"].as_str().map(str::to_owned))
            .bind(thread)
            .bind(record["run_id"].as_str().map(str::to_owned))
            .bind(record["request_id"].as_str().map(str::to_owned))
            .bind(record["otel_trace_id"].as_str().map(str::to_owned))
            .bind(record["traceparent"].as_str().map(str::to_owned))
            .bind(record["model_id"].as_str().map(str::to_owned))
            .bind(record["provider"].as_str().map(str::to_owned))
            .bind(string_or(&record, "status", "succeeded"))
            .bind(timestamp(&record["started_at"]))
            .bind(timestamp(&record["ended_at"]))
            .bind(numbers[0])
            .bind(numbers[1])
            .bind(numbers[2])
            .bind(numbers[3])
            .bind(numbers[4])
            .bind(numbers[5])
            .bind(string_or(&record, "input_preview", ""))
            .bind(string_or(&record, "output_preview", ""))
            .bind(default_object(&record, "redaction_state"))
            .bind(metadata)
            .bind(metrics)
            .bind(privacy)
            .bind(string_or(&record, "source_adapter", "api"))
            .bind(timestamp(&record["retention_expires_at"]))
            .bind(scope_user_id.map(str::to_owned));

        let mut spans = Vec::new();
        for span in trace["spans"].as_array().into_iter().flatten() {
            spans.push((
                span["parent_span_id"].as_str().map(str::to_owned),
                span_query(&id, span)?,
            ));
        }
        let mut events = Vec::new();
        for event in trace["events"].as_array().into_iter().flatten() {
            events.push((
                event["span_id"].as_str().map(str::to_owned),
                event_query(&id, event)?,
            ));
        }
        let tenant = tenant_id.to_owned();
        let runtime = trace["source_adapter"] == "gateway.agent_runtime";
        let session = trace["session_id"].as_str().map(str::to_owned);
        let job_payload = json!({"trace_id":id,"trace_family":string_or(&trace,"trace_family","assistant"),
            "status":string_or(&trace,"status","succeeded"),"source_adapter":string_or(&trace,"source_adapter","api")});
        self.run(move |conn| Box::pin(async move {
            // The trace upsert locks the existing row and checks its current
            // tenant/owner in the same statement. Children and enqueue commit
            // together, including under the repository's existing outbox fence.
            let mut tx = conn.begin().await?;
            if trace_query.fetch_optional(&mut *tx).await?.is_none() {
                tx.rollback().await?;
                return Ok(None);
            }
            if runtime {
                sqlx::query(sql::BIND_RUNTIME_DIMENSIONS).bind(&id).bind(&tenant)
                    .bind(&owner).bind(session).execute(&mut *tx).await?;
            }
            for (parent, query) in spans {
                if !span_reference_in(&mut tx, &id, parent.as_deref()).await?
                    || query.fetch_optional(&mut *tx).await?.is_none() {
                    tx.rollback().await?;
                    return Ok(None);
                }
            }
            for (span, query) in events {
                if !span_reference_in(&mut tx, &id, span.as_deref()).await?
                    || query.fetch_optional(&mut *tx).await?.is_none() {
                    tx.rollback().await?;
                    return Ok(None);
                }
            }
            let job = if enqueue {
                enqueue_in(&mut tx, &tenant, &id, job_payload, runtime).await?
            } else { None };
            tx.commit().await?;
            Ok(Some(json!({"trace_id":id,"status":"stored","job_id":job.as_ref().and_then(|v|v.get("job_id"))})))
        })).await?.ok_or_else(TraceError::not_found)
    }

    pub async fn create_trace_ingested_outbox_job(
        &self,
        tenant_id: &str,
        trace_id: &str,
        trace_family: &str,
        status: &str,
        source_adapter: &str,
    ) -> Result<Option<Value>, TraceError> {
        if self
            .claim
            .as_ref()
            .is_some_and(|claim| claim.tenant_id != tenant_id)
        {
            return Err(TraceError::lease_lost());
        }
        let runtime = source_adapter == "gateway.agent_runtime";
        let tenant = tenant_id.to_owned();
        let trace = trace_id.to_owned();
        let payload = json!({"trace_id":trace_id,"trace_family":trace_family,"status":status,"source_adapter":source_adapter});
        self.run(move |conn| Box::pin(async move {
            let mut tx = conn.begin().await?;
            let owned = sqlx::query_scalar::<_,String>("SELECT trace_id::text FROM agent_traces WHERE trace_id=$1::uuid AND tenant_id=$2 FOR UPDATE")
                .bind(&trace).bind(&tenant).fetch_optional(&mut *tx).await?;
            if owned.is_none() { tx.rollback().await?; return Ok(None); }
            let job = enqueue_in(&mut tx, &tenant, &trace, payload, runtime).await?;
            tx.commit().await?;
            Ok(job)
        })).await
    }
    pub async fn get_thread(
        &self,
        tenant_id: &str,
        thread_id: &str,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Value, TraceError> {
        let tenant = tenant_id.to_owned();
        let thread = thread_id.to_owned();
        let user = user_id.filter(|s| !s.is_empty()).map(str::to_owned);
        let rows=self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,Value>("SELECT to_jsonb(q) FROM (SELECT t.*,COUNT(s.score_id)::int AS scores_count FROM agent_traces t LEFT JOIN agent_trace_scores s ON s.trace_id=t.trace_id WHERE t.tenant_id=$1 AND COALESCE(t.thread_id,t.session_id)=$2 AND ($3::text IS NULL OR t.user_id=$3) GROUP BY t.trace_id ORDER BY t.started_at ASC,t.created_at ASC LIMIT $4) q").bind(tenant).bind(thread).bind(user).bind(limit).fetch_all(conn).await})).await?;
        let traces: Vec<Value> = rows.into_iter().map(|v| decode(v, "trace")).collect();
        let total = traces.len();
        let metrics = json!({"trace_count":total,"total_latency_ms":traces.iter().map(|v|v["total_latency_ms"].as_i64().unwrap_or(0)).sum::<i64>(),"total_tokens":traces.iter().map(|v|v["total_tokens"].as_i64().unwrap_or(0)).sum::<i64>(),"failed_count":traces.iter().filter(|v|v["status"]=="failed").count()});
        Ok(json!({"thread_id":thread_id,"traces":traces,"total":total,"metrics":metrics}))
    }
}

// Child IDs are globally unique, so their conflict updates must remain bound to
// the already-authorized parent Trace. Parent/span references are likewise local.
type IngestQuery = sqlx::query::Query<'static, sqlx::Postgres, sqlx::postgres::PgArguments>;
fn span_query(trace_id: &str, span: &Value) -> Result<IngestQuery, TraceError> {
    let id = if let Some(v) = span.get("span_id").filter(|v| truthy(v)) {
        pystr(v)
    } else {
        let namespace = uuid::Uuid::parse_str(trace_id).map_err(|_| TraceError::invalid())?;
        uuid::Uuid::new_v5(
            &namespace,
            string_or(span, "name", &uuid::Uuid::new_v4().to_string()).as_bytes(),
        )
        .to_string()
    };
    Ok(sqlx::query(sql::UPSERT_SPAN)
        .bind(id)
        .bind(trace_id.to_owned())
        .bind(span["parent_span_id"].as_str().map(str::to_owned))
        .bind(string_or(span, "span_kind", "custom"))
        .bind(
            string_or(span, "name", "span")
                .chars()
                .take(160)
                .collect::<String>(),
        )
        .bind(string_or(span, "status", "succeeded"))
        .bind(integer(&span["sequence_no"])?)
        .bind(timestamp(&span["started_at"]))
        .bind(timestamp(&span["ended_at"]))
        .bind(integer(&span["duration_ms"])?)
        .bind(string_or(span, "input_preview", ""))
        .bind(string_or(span, "output_preview", ""))
        .bind(default_object(span, "attributes"))
        .bind(span["error_type"].as_str().map(str::to_owned))
        .bind(span["error_message"].as_str().map(str::to_owned)))
}
fn event_query(trace_id: &str, event: &Value) -> Result<IngestQuery, TraceError> {
    Ok(sqlx::query(sql::UPSERT_EVENT)
        .bind(trace_id.to_owned())
        .bind(event["span_id"].as_str().map(str::to_owned))
        .bind(string_or(event, "event_type", "event"))
        .bind(integer(&event["sequence_no"])?)
        .bind(timestamp(&event["occurred_at"]))
        .bind(default_object(event, "payload"))
        .bind(integer(&event["payload_size_bytes"])?)
        .bind(event.get("redacted").map(truthy).unwrap_or(true)))
}
async fn span_reference_in(
    conn: &mut sqlx::PgConnection,
    trace: &str,
    span: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let Some(span) = span.filter(|s| !s.is_empty()) else {
        return Ok(true);
    };
    Ok(sqlx::query("SELECT span_id FROM agent_trace_spans WHERE trace_id=$1::uuid AND span_id=$2::uuid FOR KEY SHARE")
        .bind(trace).bind(span).fetch_optional(conn).await?.is_some())
}
async fn enqueue_in(
    conn: &mut sqlx::PgConnection,
    tenant: &str,
    trace: &str,
    payload: Value,
    runtime: bool,
) -> Result<Option<Value>, sqlx::Error> {
    let args = sql::enqueue_trace_arguments(tenant, payload, trace, runtime);
    sqlx::query_scalar(sql::ENQUEUE_TRACE)
        .bind(args.0)
        .bind(args.1)
        .bind(args.2)
        .bind(args.3)
        .fetch_optional(conn)
        .await
}
