//! Durable progress replay. Dropping the response cancels polling immediately;
//! no background producer or database connection survives the request.
use super::{IngestError, KnowledgeWriteStore, Result, integer, object, string};
use serde_json::Value;
use std::{collections::VecDeque, time::Duration};

pub fn parse_cursor(dataset: &str, raw: Option<&str>) -> Result<i64> {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(0);
    }
    let (scope, value) = raw.rsplit_once(':').ok_or_else(invalid_cursor)?;
    if scope != dataset {
        return Err(invalid_cursor());
    }
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(invalid_cursor)
}
fn invalid_cursor() -> IngestError {
    IngestError::Validation("Invalid document progress event cursor".into())
}
pub fn format_event(dataset: &str, event: &Value) -> Result<String> {
    let sequence = integer(event, "event_sequence");
    if sequence <= 0 {
        return Err(IngestError::Validation(
            "document progress event has an invalid sequence".into(),
        ));
    }
    let kind = string(event, "event_type").trim().to_lowercase();
    let kind = if ["progress", "terminal", "deleted"].contains(&kind.as_str()) {
        kind.as_str()
    } else {
        "progress"
    };
    let mut payload = object(&event["payload"]);
    super::diagnostics::sanitize_processing_error(&mut payload);
    Ok(format!(
        "id: {dataset}:{sequence}\nevent: {kind}\ndata: {}\n\n",
        serde_json::to_string(&payload)?
    ))
}
impl KnowledgeWriteStore {
    pub async fn progress_bounds(&self, dataset: &str) -> Result<(Option<i64>, Option<i64>)> {
        if dataset.trim().is_empty() {
            return Err(IngestError::Validation("dataset_id is required".into()));
        }
        let mut transaction = self.pool.begin().await?;
        ai_platform_service_primitives::data_connection::append_store::enabled(
            &mut transaction,
            "knowledge.kb_document_progress_events",
        )
        .await?;
        let result=sqlx::query_as("SELECT MIN(event_sequence),MAX(event_sequence) FROM knowledge.kb_document_progress_events WHERE dataset_id=$1").bind(dataset.trim()).fetch_one(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(result)
    }
    pub async fn progress_events(
        &self,
        dataset: &str,
        cursor: i64,
        document_ids: Option<&[String]>,
        limit: i64,
    ) -> Result<Vec<Value>> {
        if dataset.trim().is_empty() {
            return Err(IngestError::Validation("dataset_id is required".into()));
        }
        let ids: Vec<String> = document_ids
            .unwrap_or(&[])
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut transaction = self.pool.begin().await?;
        ai_platform_service_primitives::data_connection::append_store::enabled(
            &mut transaction,
            "knowledge.kb_document_progress_events",
        )
        .await?;
        let result=sqlx::query_scalar("SELECT to_jsonb(event) FROM(SELECT event_sequence,dataset_id,document_id,event_type,payload,created_at FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND event_sequence>$2 AND ($3::varchar[] IS NULL OR document_id=ANY($3::varchar[])) ORDER BY event_sequence ASC LIMIT $4) event").bind(dataset.trim()).bind(cursor.max(0)).bind(if ids.is_empty(){None}else{Some(ids)}).bind(limit.clamp(1,500)).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(result)
    }
    pub async fn get_pipeline_execution(&self, id: &str) -> Result<Option<Value>> {
        if id.trim().is_empty() {
            return Ok(None);
        }
        Ok(sqlx::query_scalar("SELECT to_jsonb(execution) FROM(SELECT execution_id,document_id,dataset_id,action,trigger_source,triggered_by,process_rule_id,input_snapshot,manifest,status,error,created_at,completed_at FROM knowledge.document_pipeline_executions WHERE execution_id=$1) execution").bind(id.trim()).fetch_optional(crate::database::executor(&self.pool)).await?)
    }
}
pub async fn response(
    store: KnowledgeWriteStore,
    dataset: String,
    raw: Option<&str>,
) -> Result<axum::response::Response> {
    let mut cursor = parse_cursor(&dataset, raw)?;
    let (oldest, newest) = store.progress_bounds(&dataset).await?;
    let newest = newest.unwrap_or(0);
    let mut reset = None;
    if raw.unwrap_or("").trim().is_empty() {
        cursor = newest
    } else if cursor > newest || oldest.is_some_and(|oldest| cursor.saturating_add(1) < oldest) {
        cursor = newest;
        reset = Some(newest)
    }
    store.progress_events(&dataset, cursor, None, 1).await?;
    let mut pending = VecDeque::from([": connected\nretry: 2000\n\n".to_owned()]);
    if let Some(sequence) = reset {
        pending.push_back(format!(
            "id: {dataset}:{sequence}\nevent: reset\ndata: {{\"reason\":\"cursor_expired\"}}\n\n"
        ));
    }
    struct State {
        store: KnowledgeWriteStore,
        dataset: String,
        cursor: i64,
        pending: VecDeque<String>,
        heartbeat: tokio::time::Instant,
    }
    let state = State {
        store,
        dataset,
        cursor,
        pending,
        heartbeat: tokio::time::Instant::now(),
    };
    let stream = futures_util::stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Ok::<_, std::io::Error>(Some((axum::body::Bytes::from(event), state)));
            }
            let events = state
                .store
                .progress_events(&state.dataset, state.cursor, None, 200)
                .await
                .map_err(|_| std::io::Error::other("document progress stream read failed"))?;
            for event in events {
                let sequence = integer(&event, "event_sequence");
                if sequence <= state.cursor {
                    continue;
                }
                state.cursor = sequence;
                state.pending.push_back(
                    format_event(&state.dataset, &event)
                        .map_err(|_| std::io::Error::other("document progress event invalid"))?,
                );
            }
            if !state.pending.is_empty() {
                continue;
            }
            if state.heartbeat.elapsed() >= Duration::from_secs(15) {
                state.heartbeat = tokio::time::Instant::now();
                return Ok(Some((
                    axum::body::Bytes::from_static(b": keep-alive\n\n"),
                    state,
                )));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    axum::response::Response::builder()
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|_| IngestError::Storage)
}
