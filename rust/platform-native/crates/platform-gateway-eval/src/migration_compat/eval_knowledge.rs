//! Knowledge RAGAS summary, bounded queue fan-out, and signed retrieval scoring.
use super::{
    common::{CompatError, CompatUser, text},
    eval::EvalService,
    eval_http::EvalHttpOptions,
};
use crate::migration_trace::TraceFilter;
use serde_json::{Value, json};
const LATEST: &str = "in_window_traces AS (SELECT t.trace_id FROM agent_traces t WHERE t.tenant_id=$1 AND t.trace_family='rag' AND t.created_at>=NOW()-make_interval(days=>$2::int) AND ($3::text IS NULL OR t.metadata->>'dataset_id'=$3)),latest_scores AS (SELECT ranked_scores.* FROM (SELECT s.*,ROW_NUMBER() OVER(PARTITION BY s.trace_id,s.evaluator_id,COALESCE(s.evaluator_version,''),s.score_name ORDER BY s.created_at DESC,s.score_id DESC) AS score_revision FROM agent_trace_scores s INNER JOIN in_window_traces t ON t.trace_id=s.trace_id WHERE s.score_source='kb_ragas') ranked_scores WHERE ranked_scores.score_revision=1)";
impl EvalService {
    pub async fn kb_ragas_summary(
        &self,
        tenant: &str,
        days: i32,
        dataset: Option<&str>,
    ) -> Result<Value, CompatError> {
        let dataset = dataset.filter(|s| !s.is_empty());
        let counts=sqlx::query_scalar::<_,Value>(&format!("WITH {LATEST} SELECT jsonb_build_object('rag_traces',(SELECT COUNT(DISTINCT trace_id) FROM in_window_traces),'ragas_scored_traces',COUNT(DISTINCT CASE WHEN s.label IN ('pass','fail') THEN s.trace_id END)) FROM latest_scores s")).bind(tenant).bind(days.max(1)).bind(dataset).fetch_one(&self.pool).await?;
        let metrics=sqlx::query_scalar::<_,Value>(&format!("WITH {LATEST} SELECT to_jsonb(q) FROM (SELECT s.score_name AS metric,COALESCE(AVG(s.numeric_value) FILTER(WHERE s.label IN ('pass','fail')),0)::float AS average_score,COUNT(*) FILTER(WHERE s.label IN ('pass','fail'))::int AS scored_count,COUNT(*) FILTER(WHERE s.label='pass')::int AS pass_count,COUNT(*) FILTER(WHERE s.label='fail')::int AS fail_count,COUNT(*) FILTER(WHERE s.label='review')::int AS review_count FROM latest_scores s GROUP BY s.score_name ORDER BY s.score_name) q")).bind(tenant).bind(days.max(1)).bind(dataset).fetch_all(&self.pool).await?;
        let judge=sqlx::query_scalar::<_,String>(&format!("WITH {LATEST} SELECT s.metadata->>'judge_model' FROM latest_scores s WHERE COALESCE(s.metadata->>'judge_model','')<>'' ORDER BY s.created_at DESC,s.score_id DESC LIMIT 1")).bind(tenant).bind(days.max(1)).bind(dataset).fetch_optional(&self.pool).await?;
        Ok(
            json!({"window_days":days.max(1),"dataset_id":dataset,"rag_traces":counts["rag_traces"],"ragas_scored_traces":counts["ragas_scored_traces"],"metrics":metrics,"latest_judge_model":judge}),
        )
    }
    pub async fn kb_ragas_batch(
        &self,
        user: &CompatUser,
        dataset: &str,
        body: &Value,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        let evaluator = text(body, "evaluator_id");
        let info = self
            .get(&user.tenant_id, "evaluators", evaluator)
            .await?
            .ok_or_else(|| CompatError::new(400, "Evaluator not found"))?;
        if info["evaluator_type"] != "ragas" {
            return Err(CompatError::new(400, "Evaluator must be of type ragas"));
        }
        let limit = body
            .get("limit")
            .map(|v| {
                v.as_i64()
                    .filter(|v| (1..=200).contains(v))
                    .ok_or_else(|| CompatError::new(422, "Invalid limit"))
            })
            .transpose()?
            .unwrap_or(50);
        let unscored = body
            .get("only_unscored")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| CompatError::new(422, "Invalid only_unscored"))
            })
            .transpose()?
            .unwrap_or(true);
        let repo = options
            .traces
            .as_ref()
            .ok_or_else(|| CompatError::new(503, "Database not available"))?;
        let page = limit.clamp(1, 200);
        let (mut offset, mut skipped, mut jobs) = (0, 0, vec![]);
        while jobs.len() < limit as usize {
            let (rows, total) = repo
                .list_traces(
                    &user.tenant_id,
                    &TraceFilter {
                        trace_family: "rag".into(),
                        metadata_dataset_id: Some(dataset.into()),
                        limit: page,
                        offset,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| CompatError::new(e.status, e.code))?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                if jobs.len() >= limit as usize {
                    break;
                }
                let trace = text(&row, "trace_id");
                if trace.is_empty() {
                    skipped += 1;
                    continue;
                }
                if unscored&&sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM agent_trace_scores s INNER JOIN agent_traces t ON t.trace_id=s.trace_id WHERE t.tenant_id=$1 AND s.trace_id=$2::text::uuid AND s.evaluator_id=$3::text::uuid AND s.score_source='kb_ragas' AND s.label IN ('pass','fail'))").bind(&user.tenant_id).bind(trace).bind(evaluator).fetch_one(&self.pool).await?{skipped+=1;continue;}
                if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM eval_experiment_runs WHERE tenant_id=$1 AND evaluator_id=$2::text::uuid AND status IN ('queued','running') AND target_snapshot->>'trace_id'=$3)").bind(&user.tenant_id).bind(evaluator).bind(trace).fetch_one(&self.pool).await?{skipped+=1;continue;}
                jobs.push(self.enqueue_evaluator_run(user,&json!({"evaluator_id":evaluator,"trace_id":trace,"target_snapshot":{"trace_id":trace,"trace_family":"rag","source":"kb_ragas_batch","dataset_id":dataset},"metadata":{"kb_ragas_batch":true,"dataset_id":dataset}})).await?);
            }
            offset += page;
            if offset >= total {
                break;
            }
        }
        Ok(json!({"queued":jobs.len(),"skipped":skipped,"jobs":jobs}))
    }
}
pub async fn score_retrieval(
    user: &CompatUser,
    body: &Value,
    options: &EvalHttpOptions,
) -> Result<Value, CompatError> {
    let query = body["query"]
        .as_str()
        .filter(|v| !v.is_empty() && v.chars().count() <= 4000)
        .ok_or_else(|| CompatError::new(422, "Invalid query"))?;
    let strings = |key: &str| {
        body[key]
            .as_array()
            .ok_or_else(|| CompatError::new(422, format!("Invalid {key}")))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| CompatError::new(422, format!("Invalid {key}")))
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let contexts = strings("contexts")?;
    if !(1..=32).contains(&contexts.len()) {
        return Err(CompatError::new(422, "Invalid contexts"));
    }
    for key in ["answer", "ground_truth"] {
        if body
            .get(key)
            .filter(|v| !v.is_null())
            .is_some_and(|v| v.as_str().is_none_or(|s| s.chars().count() > 8000))
        {
            return Err(CompatError::new(422, format!("Invalid {key}")));
        }
    }
    let metrics = if body["metrics"].is_null() {
        None
    } else {
        Some(strings("metrics")?)
    };
    let client = options
        .ragas
        .as_ref()
        .ok_or_else(|| CompatError::new(503, "KB RAGAS client unavailable"))?;
    let result = client
        .evaluate_retrieval_scoped(
            &super::eval_clients::ragas_actor::scope(user)?,
            query,
            &contexts,
            body["answer"].as_str(),
            metrics.as_deref(),
            body["ground_truth"].as_str(),
            body.get("llm_config"),
        )
        .await?;
    Ok(
        json!({"judge_model":text(&result,"judge_model"),"results":result["results"].as_array().map(|items|items.iter().map(|v|json!({"metric":v["metric"],"score":v["score"],"explanation":v["explanation"],"label":v["label"]})).collect::<Vec<_>>()).unwrap_or_default()}),
    )
}
