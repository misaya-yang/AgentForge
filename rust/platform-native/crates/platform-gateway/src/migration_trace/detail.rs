use super::*;
use std::collections::{BTreeMap, BTreeSet};
impl TraceRepository {
    pub async fn get_trace_detail(
        &self,
        tenant_id: &str,
        trace_id: &str,
        user_id: Option<&str>,
        trace_family: &str,
    ) -> Result<Option<Value>, TraceError> {
        let tenant = tenant_id.to_owned();
        let id = trace_id.to_owned();
        let user = user_id.filter(|s| !s.is_empty()).map(str::to_owned);
        let family = trace_family.to_owned();
        let trace=self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,Value>("SELECT to_jsonb(t)||jsonb_build_object('scores_count',0) FROM agent_traces t WHERE trace_id=$1::uuid AND tenant_id=$2 AND trace_family=$3 AND ($4::text IS NULL OR user_id=$4)").bind(id).bind(tenant).bind(family).bind(user).fetch_optional(conn).await})).await?;
        let Some(trace) = trace else {
            return Ok(None);
        };
        let mut detail = json!({"trace":decode(trace,"trace"),"spans":[],"events":[],"scores":[]});
        for (name, table, order, kind) in [
            (
                "spans",
                "agent_trace_spans",
                "sequence_no ASC,started_at ASC",
                "span",
            ),
            (
                "events",
                "agent_trace_events",
                "sequence_no ASC,occurred_at ASC",
                "event",
            ),
            ("scores", "agent_trace_scores", "created_at DESC", "score"),
        ] {
            let id = trace_id.to_owned();
            let sql = format!(
                "SELECT to_jsonb(t) FROM {table} t WHERE trace_id=$1::uuid ORDER BY {order}"
            );
            let rows = self
                .run(move |conn| {
                    Box::pin(async move {
                        sqlx::query_scalar::<_, Value>(&sql)
                            .bind(id)
                            .fetch_all(conn)
                            .await
                    })
                })
                .await?;
            detail[name] = Value::Array(rows.into_iter().map(|v| decode(v, kind)).collect());
        }
        detail["trace"]["scores_count"] = json!(detail["scores"].as_array().map_or(0, Vec::len));
        Ok(Some(detail))
    }
    pub async fn get_trace_details(
        &self,
        tenant_id: &str,
        trace_ids: &[String],
        user_id: Option<&str>,
        trace_family: &str,
    ) -> Result<BTreeMap<String, Value>, TraceError> {
        let mut seen = BTreeSet::new();
        let ids: Vec<String> = trace_ids
            .iter()
            .filter(|id| !id.is_empty() && seen.insert((*id).clone()))
            .cloned()
            .collect();
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let tenant = tenant_id.to_owned();
        let requested = ids.clone();
        let user = user_id.filter(|s| !s.is_empty()).map(str::to_owned);
        let family = trace_family.to_owned();
        let traces=self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,Value>("SELECT to_jsonb(t)||jsonb_build_object('scores_count',0) FROM agent_traces t WHERE trace_id=ANY($1::text[]::uuid[]) AND tenant_id=$2 AND trace_family=$3 AND ($4::text IS NULL OR user_id=$4) ORDER BY array_position($1::text[]::uuid[],trace_id)").bind(requested).bind(tenant).bind(family).bind(user).fetch_all(conn).await})).await?;
        let mut details: BTreeMap<String, Value> = traces
            .into_iter()
            .map(|trace| {
                (
                    text(&trace, "trace_id"),
                    json!({"trace":decode(trace,"trace"),"spans":[],"events":[],"scores":[]}),
                )
            })
            .collect();
        if details.is_empty() {
            return Ok(details);
        }
        let allowed: Vec<String> = details.keys().cloned().collect();
        for (name, table, order, kind) in [
            (
                "spans",
                "agent_trace_spans",
                "trace_id,sequence_no ASC,started_at ASC",
                "span",
            ),
            (
                "events",
                "agent_trace_events",
                "trace_id,sequence_no ASC,occurred_at ASC",
                "event",
            ),
            (
                "scores",
                "agent_trace_scores",
                "trace_id,created_at DESC",
                "score",
            ),
        ] {
            let allowed = allowed.clone();
            let sql = format!(
                "SELECT to_jsonb(t) FROM {table} t WHERE trace_id=ANY($1::text[]::uuid[]) ORDER BY {order}"
            );
            let rows = self
                .run(move |conn| {
                    Box::pin(async move {
                        sqlx::query_scalar::<_, Value>(&sql)
                            .bind(allowed)
                            .fetch_all(conn)
                            .await
                    })
                })
                .await?;
            for row in rows {
                if let Some(detail) = details.get_mut(&text(&row, "trace_id")) {
                    detail[name]
                        .as_array_mut()
                        .expect("array initialized")
                        .push(decode(row, kind));
                }
            }
        }
        for detail in details.values_mut() {
            detail["trace"]["scores_count"] =
                json!(detail["scores"].as_array().map_or(0, Vec::len));
        }
        Ok(details)
    }
    pub async fn trace_has_kb_ragas_score(
        &self,
        tenant_id: &str,
        trace_id: &str,
        evaluator_id: Option<&str>,
    ) -> Result<bool, TraceError> {
        let tenant = tenant_id.to_owned();
        let id = trace_id.to_owned();
        let evaluator = evaluator_id.filter(|s| !s.is_empty()).map(str::to_owned);
        self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,i32>("SELECT 1 FROM agent_trace_scores s INNER JOIN agent_traces t ON t.trace_id=s.trace_id WHERE s.trace_id=$2::uuid AND t.tenant_id=$1 AND s.score_source='kb_ragas' AND s.label IN ('pass','fail') AND ($3::text IS NULL OR s.evaluator_id=$3::uuid) LIMIT 1").bind(tenant).bind(id).bind(evaluator).fetch_optional(conn).await.map(|v|v.is_some())})).await
    }
    pub async fn purge_expired_traces(
        &self,
        default_retention_days: i32,
        batch_size: i64,
    ) -> Result<usize, TraceError> {
        self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,String>("WITH doomed AS (SELECT trace_id FROM agent_traces WHERE COALESCE(retention_expires_at,created_at+make_interval(days=>$1::int))<NOW() ORDER BY created_at ASC LIMIT $2) DELETE FROM agent_traces t USING doomed d WHERE t.trace_id=d.trace_id RETURNING t.trace_id::text").bind(default_retention_days.max(1)).bind(batch_size.max(1)).fetch_all(conn).await.map(|v|v.len())})).await
    }
}
