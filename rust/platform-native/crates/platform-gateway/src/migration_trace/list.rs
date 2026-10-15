use super::summary::Param;
use super::*;
use chrono::{DateTime, Utc};
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
pub struct TraceFilter {
    pub user_id: Option<String>,
    pub trace_family: String,
    pub status: Option<String>,
    pub model_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub request_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_version_id: Option<String>,
    pub publication_id: Option<String>,
    pub channel: Option<String>,
    pub transcript_query: Option<String>,
    pub turn_index: Option<i64>,
    pub span_kind: Option<String>,
    pub score_name: Option<String>,
    pub score_label: Option<String>,
    pub min_score: Option<f64>,
    pub max_score: Option<f64>,
    pub min_latency_ms: Option<i64>,
    pub max_latency_ms: Option<i64>,
    pub dataset_id: Option<String>,
    pub metadata_dataset_id: Option<String>,
    pub started_after: Option<DateTime<Utc>>,
    pub started_before: Option<DateTime<Utc>>,
    pub limit: i64,
    pub offset: i64,
}
impl Default for TraceFilter {
    fn default() -> Self {
        Self {
            user_id: None,
            trace_family: "assistant".into(),
            status: None,
            model_id: None,
            session_id: None,
            run_id: None,
            request_id: None,
            agent_id: None,
            agent_version_id: None,
            publication_id: None,
            channel: None,
            transcript_query: None,
            turn_index: None,
            span_kind: None,
            score_name: None,
            score_label: None,
            min_score: None,
            max_score: None,
            min_latency_ms: None,
            max_latency_ms: None,
            dataset_id: None,
            metadata_dataset_id: None,
            started_after: None,
            started_before: None,
            limit: 50,
            offset: 0,
        }
    }
}
impl TraceRepository {
    pub async fn list_traces(
        &self,
        tenant_id: &str,
        filter: &TraceFilter,
    ) -> Result<(Vec<Value>, i64), TraceError> {
        let mut params = vec![
            Param::Text(tenant_id.into()),
            Param::Text(filter.trace_family.clone()),
        ];
        let mut filters = vec!["t.tenant_id = $1".into(), "t.trace_family = $2".into()];
        for (column, value, cast) in [
            ("user_id", &filter.user_id, ""),
            ("status", &filter.status, ""),
            ("model_id", &filter.model_id, ""),
            ("session_id", &filter.session_id, ""),
            ("run_id", &filter.run_id, ""),
            ("request_id", &filter.request_id, ""),
            ("agent_id", &filter.agent_id, "::uuid"),
            ("agent_version_id", &filter.agent_version_id, "::uuid"),
            ("publication_id", &filter.publication_id, "::uuid"),
            ("channel", &filter.channel, ""),
        ] {
            if let Some(v) = value.as_ref().filter(|s| !s.is_empty()) {
                params.push(Param::Text(v.clone()));
                filters.push(format!("t.{column} = ${}{cast}", params.len()));
            }
        }
        if let Some(index) = filter.turn_index {
            params.push(Param::Text(index.to_string()));
            filters.push(format!(
                "t.metadata->'transcript_locator'->>'turn_index' = ${}",
                params.len()
            ));
        }
        if let Some(query) = filter.transcript_query.as_ref().filter(|s| !s.is_empty()) {
            params.push(Param::Text(format!("%{query}%")));
            let p = params.len();
            let mut clauses = vec![format!("t.trace_id::text ILIKE ${p}")];
            for column in [
                "request_id",
                "run_id",
                "session_id",
                "user_id",
                "model_id",
                "provider",
                "input_preview",
                "output_preview",
            ] {
                clauses.push(format!("COALESCE(t.{column},'') ILIKE ${p}"));
            }
            for key in ["current_message_preview", "transcript_excerpt", "turn_id"] {
                clauses.push(format!(
                    "COALESCE(t.metadata->'transcript_locator'->>'{key}','') ILIKE ${p}"
                ));
            }
            filters.push(format!("({})", clauses.join(" OR ")));
        }
        for (value, table, alias, column, cast) in [
            (
                &filter.span_kind,
                "agent_trace_spans",
                "st",
                "span_kind",
                "",
            ),
            (
                &filter.score_name,
                "agent_trace_scores",
                "ss",
                "score_name",
                "",
            ),
            (&filter.score_label, "agent_trace_scores", "sl", "label", ""),
            (
                &filter.dataset_id,
                "eval_examples",
                "ex",
                "dataset_id",
                "::uuid",
            ),
        ] {
            if let Some(v) = value.as_ref().filter(|s| !s.is_empty()) {
                params.push(Param::Text(v.clone()));
                let trace_column = if alias == "ex" {
                    "source_trace_id"
                } else {
                    "trace_id"
                };
                filters.push(format!("EXISTS (SELECT 1 FROM {table} {alias} WHERE {alias}.{trace_column} = t.trace_id AND {alias}.{column} = ${}{cast})",params.len()));
            }
        }
        for (op, value) in [(">=", filter.min_latency_ms), ("<=", filter.max_latency_ms)] {
            if let Some(v) = value {
                params.push(Param::Int(v));
                filters.push(format!("t.total_latency_ms {op} ${}", params.len()));
            }
        }
        for (op, value) in [(">=", filter.started_after), ("<=", filter.started_before)] {
            if let Some(v) = value {
                params.push(Param::Time(v));
                filters.push(format!("t.started_at {op} ${}", params.len()));
            }
        }
        for (alias, op, value) in [
            ("smn", ">=", filter.min_score),
            ("smx", "<=", filter.max_score),
        ] {
            if let Some(v) = value {
                params.push(Param::Float(v));
                filters.push(format!("EXISTS (SELECT 1 FROM agent_trace_scores {alias} WHERE {alias}.trace_id = t.trace_id AND {alias}.numeric_value {op} ${})",params.len()));
            }
        }
        if let Some(v) = filter
            .metadata_dataset_id
            .as_ref()
            .filter(|s| !s.is_empty())
        {
            params.push(Param::Text(v.clone()));
            filters.push(format!("t.metadata->>'dataset_id' = ${}", params.len()));
        }
        let where_clause = filters.join(" AND ");
        let count = self
            .query_rows(
                format!("SELECT COUNT(*) AS total FROM agent_traces t WHERE {where_clause}"),
                params.clone(),
            )
            .await?;
        let total = count.first().and_then(|v| v["total"].as_i64()).unwrap_or(0);
        let size = params.len();
        params.push(Param::Int(filter.limit));
        params.push(Param::Int(filter.offset));
        let rows=self.query_rows(format!("SELECT t.*, COUNT(s.score_id)::int AS scores_count FROM agent_traces t LEFT JOIN agent_trace_scores s ON s.trace_id=t.trace_id WHERE {where_clause} GROUP BY t.trace_id ORDER BY t.created_at DESC LIMIT ${} OFFSET ${}",size+1,size+2),params).await?;
        Ok((
            rows.into_iter().map(|v| decode(v, "trace")).collect(),
            total,
        ))
    }
}
