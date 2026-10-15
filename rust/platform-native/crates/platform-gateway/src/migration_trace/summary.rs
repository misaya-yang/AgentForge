use super::summary_sql as queries;
use super::*;
use chrono::{DateTime, Utc};
#[derive(Clone)]
pub(super) enum Param {
    Text(String),
    Int(i64),
    Float(f64),
    Time(DateTime<Utc>),
}
#[derive(Clone, Default, serde::Deserialize)]
pub struct AgentOperationsFilter {
    pub agent_version_id: Option<String>,
    pub publication_id: Option<String>,
    pub channel: Option<String>,
    pub started_after: Option<DateTime<Utc>>,
    pub started_before: Option<DateTime<Utc>>,
}
impl TraceRepository {
    pub(super) async fn query_rows(
        &self,
        sql: String,
        params: Vec<Param>,
    ) -> Result<Vec<Value>, TraceError> {
        self.run(move |conn| {
            Box::pin(async move {
                let wrapped = format!("SELECT to_jsonb(result) FROM ({sql}) result");
                let mut query = sqlx::query_scalar::<_, Value>(&wrapped);
                for value in params {
                    query = match value {
                        Param::Text(v) => query.bind(v),
                        Param::Int(v) => query.bind(v),
                        Param::Float(v) => query.bind(v),
                        Param::Time(v) => query.bind(v),
                    }
                }
                query.fetch_all(conn).await
            })
        })
        .await
    }
    pub async fn get_agent_operations_summary(
        &self,
        tenant_id: &str,
        agent_id: &str,
        filter: &AgentOperationsFilter,
    ) -> Result<Value, TraceError> {
        let base = vec![Param::Text(tenant_id.into()), Param::Text(agent_id.into())];
        let mut params = base.clone();
        let mut filters: Vec<String> = vec!["tenant_id = $1".into(), "agent_id = $2::uuid".into()];
        for (column, value, cast) in [
            ("agent_version_id", &filter.agent_version_id, "::uuid"),
            ("publication_id", &filter.publication_id, "::uuid"),
            ("channel", &filter.channel, ""),
        ] {
            if let Some(v) = value.as_ref().filter(|s| !s.is_empty()) {
                params.push(Param::Text(v.clone()));
                filters.push(format!("{column} = ${}{cast}", params.len()));
            }
        }
        for (op, value) in [(">=", filter.started_after), ("<=", filter.started_before)] {
            if let Some(v) = value {
                params.push(Param::Time(v));
                filters.push(format!("started_at {op} ${}", params.len()));
            }
        }
        let where_clause = filters.join(" AND ");
        let mut result = self
            .query_rows(
                queries::OPS_0.replace("{{where_clause}}", &where_clause),
                params.clone(),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        let breakdown = self
            .query_rows(
                queries::OPS_1.replace("{{where_clause}}", &where_clause),
                params,
            )
            .await?;
        let retention = self
            .query_rows(queries::OPS_2.into(), base)
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        for (key, numerator, denominator) in [
            ("success_rate", "succeeded_runs", "total_runs"),
            ("tool_success_rate", "tool_succeeded", "tool_calls"),
            ("knowledge_hit_rate", "knowledge_hits", "knowledge_queries"),
            (
                "feedback_positive_rate",
                "positive_feedback_count",
                "feedback_count",
            ),
        ] {
            let n = result[numerator].as_f64().unwrap_or(0.);
            let d = result[denominator].as_f64().unwrap_or(0.);
            result[key] = if d == 0. { Value::Null } else { json!(n / d) };
        }
        result["breakdown"] =
            Value::Array(breakdown.into_iter().map(|v| decode(v, "trace")).collect());
        result["retention_limited"] = json!(truthy(&retention["last_retention_cleanup_at"]));
        result["retention"] = decode(retention, "trace");
        Ok(decode(result, "trace"))
    }
    pub async fn get_summary(
        &self,
        tenant_id: &str,
        user_id: Option<&str>,
        days: i32,
    ) -> Result<Value, TraceError> {
        let mut params = vec![
            Param::Text(tenant_id.into()),
            Param::Int(days.max(1) as i64),
        ];
        let mut filters: Vec<String> = vec![
            "tenant_id = $1".into(),
            "created_at >= NOW() - make_interval(days => $2::int)".into(),
        ];
        if let Some(user) = user_id.filter(|s| !s.is_empty()) {
            params.push(Param::Text(user.into()));
            filters.push("user_id = $3".into());
        }
        let mut result = self
            .query_rows(
                queries::SUMMARY_0.replace("{{where_clause}}", &filters.join(" AND ")),
                params.clone(),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        let scored = self
            .query_rows(
                queries::SUMMARY_1.replace(
                    "{{where_clause}}",
                    &filters
                        .iter()
                        .map(|s| format!("t.{s}"))
                        .collect::<Vec<_>>()
                        .join(" AND "),
                ),
                params,
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        result["scored_traces"] = json!(scored["scored_traces"].as_i64().unwrap_or(0));
        result["window_days"] = json!(days.max(1));
        Ok(result)
    }
    pub async fn get_kb_ragas_summary(
        &self,
        tenant_id: &str,
        days: i32,
        dataset_id: Option<&str>,
    ) -> Result<Value, TraceError> {
        let mut params = vec![
            Param::Text(tenant_id.into()),
            Param::Int(days.max(1) as i64),
        ];
        let mut filters: Vec<String> = vec![
            "t.tenant_id = $1".into(),
            "t.trace_family = 'rag'".into(),
            "t.created_at >= NOW() - make_interval(days => $2::int)".into(),
        ];
        if let Some(dataset) = dataset_id.filter(|s| !s.is_empty()) {
            params.push(Param::Text(dataset.into()));
            filters.push("t.metadata->>'dataset_id' = $3".into());
        }
        let ctes = queries::RAGAS_CTES.replace("{{where_clause}}", &filters.join(" AND "));
        let counts = self
            .query_rows(
                queries::RAGAS_0.replace("{{latest_scores_ctes}}", &ctes),
                params.clone(),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        let metrics = self
            .query_rows(
                queries::RAGAS_1.replace("{{latest_scores_ctes}}", &ctes),
                params.clone(),
            )
            .await?;
        let judge = self
            .query_rows(
                queries::RAGAS_2.replace("{{latest_scores_ctes}}", &ctes),
                params,
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        Ok(
            json!({"window_days":days.max(1),"dataset_id":dataset_id,"rag_traces":counts["rag_traces"].as_i64().unwrap_or(0),"ragas_scored_traces":counts["ragas_scored_traces"].as_i64().unwrap_or(0),"metrics":metrics,"latest_judge_model":judge["judge_model"]}),
        )
    }
}
