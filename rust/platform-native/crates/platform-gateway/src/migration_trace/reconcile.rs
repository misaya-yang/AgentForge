use super::reconcile_sql as queries;
use super::summary::Param;
use super::*;
use chrono::{DateTime, Utc};
#[derive(Clone, serde::Deserialize)]
pub struct RuntimeTraceScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub runtime_thread_id: String,
    pub run_id: String,
}
impl RuntimeTraceScope {
    fn params(&self) -> Vec<Param> {
        [
            &self.runtime_thread_id,
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.run_id,
        ]
        .into_iter()
        .map(|s| Param::Text(s.clone()))
        .collect()
    }
}
impl TraceRepository {
    pub async fn list_assistant_runtime_trace_reconciliation_candidates(
        &self,
        since: DateTime<Utc>,
        limit: i64,
        after_finished_at: Option<DateTime<Utc>>,
        after_run_id: Option<&str>,
    ) -> Result<Vec<Value>, TraceError> {
        let id = after_run_id.map(str::to_owned);
        self.run(move |c| {
            Box::pin(async move {
                let q = format!(
                    "SELECT to_jsonb(selected) FROM ({}) selected",
                    queries::CANDIDATES_0
                );
                sqlx::query_scalar::<_, Value>(&q)
                    .bind(since)
                    .bind(after_finished_at)
                    .bind(&id)
                    .bind(limit.clamp(1, 500))
                    .fetch_all(c)
                    .await
            })
        })
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|mut row| {
                    for key in ["usage", "snapshot"] {
                        if let Some(raw) = row[key].as_str() {
                            row[key] = serde_json::from_str(raw).unwrap_or(json!({}));
                        }
                        if !row[key].is_object() {
                            row[key] = json!({});
                        }
                    }
                    row
                })
                .collect()
        })
    }
    pub async fn get_assistant_runtime_trace_terminal(
        &self,
        scope: &RuntimeTraceScope,
    ) -> Result<Option<Value>, TraceError> {
        Ok(self
            .query_rows(queries::TERMINAL_0.into(), scope.params())
            .await?
            .into_iter()
            .next())
    }
    pub async fn count_assistant_runtime_trace_events(
        &self,
        scope: &RuntimeTraceScope,
    ) -> Result<Value, TraceError> {
        let rows = self
            .query_rows(queries::COUNTS_0.into(), scope.params())
            .await?;
        let mut result = json!({});
        for row in rows {
            let raw_kind = text(&row, "event_type");
            let kind = raw_kind.strip_prefix("compat/v1/").unwrap_or(&raw_kind);
            result[kind] = row["event_count"].clone();
        }
        Ok(result)
    }
    pub async fn read_assistant_runtime_trace_text_page(
        &self,
        scope: &RuntimeTraceScope,
        after_sequence: i64,
        limit: i64,
    ) -> Result<Vec<Value>, TraceError> {
        let mut params = scope.params();
        params.push(Param::Int(after_sequence));
        params.push(Param::Int(limit.clamp(1, 500)));
        self.query_rows(queries::TEXT_0.into(), params).await
    }
    pub async fn get_assistant_runtime_trace_model_usage(
        &self,
        scope: &RuntimeTraceScope,
    ) -> Result<Value, TraceError> {
        let params = [
            &scope.tenant_id,
            &scope.user_id,
            &scope.session_id,
            &scope.runtime_thread_id,
            &scope.run_id,
        ]
        .into_iter()
        .map(|s| Param::Text(s.clone()))
        .collect();
        Ok(self
            .query_rows(queries::USAGE_0.into(), params)
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({})))
    }
    pub async fn insert_reconciled_assistant_runtime_trace(
        &self,
        run: &Value,
        terminal: &Value,
        trace: &Value,
    ) -> Result<bool, TraceError> {
        let (run, terminal, trace) = (run.clone(), terminal.clone(), trace.clone());
        self.run(move|c|Box::pin(async move{
   let mut tx=c.begin().await?;let c=&mut *tx;let id=text(&run,"run_id");let tenant=text(&run,"tenant_id");let user=text(&run,"user_id");let session=text(&run,"session_id");
   let current=sqlx::query(queries::INSERT_0).bind(&id).bind(&tenant).bind(&user).bind(&session).bind(text(&run,"runtime_thread_id")).bind(text(&run,"status")).bind(text(&terminal,"event_id")).fetch_optional(&mut *c).await?;if current.is_none(){tx.rollback().await?;return Ok(false);}
   let metrics=&trace["metrics"];let mut metadata=json!({"schema_version":"ate-03"});if let Some(m)=trace["metadata"].as_object(){metadata.as_object_mut().unwrap().extend(m.clone());}
   let inserted=sqlx::query(queries::INSERT_1).bind(&id).bind(&tenant).bind(&user).bind(&session).bind(&id).bind(text(&trace,"request_id")).bind(trace["model_id"].as_str()).bind(trace["provider"].as_str()).bind(text(&trace,"status")).bind(super::ingest::timestamp(&trace["started_at"])).bind(super::ingest::timestamp(&trace["ended_at"])).bind(metrics["total_latency_ms"].as_i64().unwrap_or(0)).bind(metrics["first_token_latency_ms"].as_i64().unwrap_or(0)).bind(metrics["input_tokens"].as_i64().unwrap_or(0)).bind(metrics["output_tokens"].as_i64().unwrap_or(0)).bind(metrics["total_tokens"].as_i64().unwrap_or(0)).bind(text(&trace,"input_preview")).bind(text(&trace,"output_preview")).bind(&trace["redaction_state"]).bind(metadata).bind(metrics).bind(&trace["privacy"]).bind(super::ingest::timestamp(&trace["retention_expires_at"])).fetch_optional(&mut *c).await?.is_some();
   if !inserted&&sqlx::query(queries::INSERT_2).bind(&id).bind(&tenant).bind(&user).bind(&session).bind(&id).bind(text(&trace,"status")).fetch_optional(&mut *c).await?.is_none(){tx.rollback().await?;return Ok(false);}
   sqlx::query(sql::BIND_RUNTIME_DIMENSIONS).bind(&id).bind(&tenant).bind(&user).bind(&session).execute(&mut *c).await?;
   let span=&trace["spans"][0];sqlx::query(queries::INSERT_3).bind(text(span,"span_id")).bind(&id).bind(text(span,"span_kind")).bind(text(span,"name")).bind(text(span,"status")).bind(span["sequence_no"].as_i64().unwrap_or(0)).bind(super::ingest::timestamp(&span["started_at"])).bind(super::ingest::timestamp(&span["ended_at"])).bind(span["duration_ms"].as_i64().unwrap_or(0)).bind(text(span,"input_preview")).bind(text(span,"output_preview")).bind(&span["attributes"]).bind(span["error_type"].as_str()).execute(&mut *c).await?;
   let event=&trace["events"][0];sqlx::query(queries::INSERT_4).bind(text(&terminal,"event_id")).bind(&id).bind(text(event,"event_type")).bind(event["sequence_no"].as_i64().unwrap_or(1)).bind(super::ingest::timestamp(&terminal["created_at"])).bind(&event["payload"]).execute(&mut *c).await?;
   sqlx::query(queries::INSERT_5).bind(&tenant).bind(json!({"trace_id":id,"trace_family":"assistant","status":trace["status"],"source_adapter":"gateway.agent_runtime"})).bind(&id).fetch_optional(&mut *c).await?;tx.commit().await?;Ok(inserted)
  })).await
    }
}
