use super::*;
pub fn experiment_score_id(
    tenant: &str,
    trace: &str,
    family: &str,
    payload: &Value,
) -> Option<String> {
    let metadata = payload.get("metadata")?.as_object()?;
    let run = metadata
        .get("experiment_run_id")
        .filter(|v| truthy(v))
        .map(pystr)
        .unwrap_or_default();
    let run = run.trim();
    if run.is_empty() {
        return None;
    }
    let span = string_or(payload, "span_id", "");
    let target = string_or(
        payload,
        "target_id",
        if span.is_empty() { trace } else { &span },
    );
    let target_type = string_or(
        payload,
        "target_type",
        if span.is_empty() { "trace" } else { "span" },
    );
    let scorer = string_or(payload, "scorer_type", "human");
    let identity = json!({"evaluator_id":string_or(payload,"evaluator_id",""),"evaluator_name":string_or(payload,"evaluator_name",""),"evaluator_version":string_or(payload,"evaluator_version",""),"experiment_run_id":run,"score_name":string_or(payload,"score_name",""),"score_source":string_or(payload,"score_source",&scorer),"scorer_type":scorer,"span_id":span,"target_id":target,"target_type":target_type,"tenant_id":tenant,"trace_family":family,"trace_id":trace});
    let encoded = crate::migration_content::python_json(&identity, true, false);
    Some(
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("ai-gateway/eval-score/{encoded}").as_bytes(),
        )
        .to_string(),
    )
}
impl TraceRepository {
    pub async fn create_score(
        &self,
        tenant_id: &str,
        trace_id: &str,
        created_by: &str,
        payload: &Value,
        user_id: Option<&str>,
        trace_family: &str,
    ) -> Result<Option<Value>, TraceError> {
        let tenant = tenant_id.to_owned();
        let trace = trace_id.to_owned();
        let family = trace_family.to_owned();
        let user = user_id.filter(|s| !s.is_empty()).map(str::to_owned);
        let exists=self.run(move|conn|Box::pin(async move{sqlx::query_scalar::<_,String>("SELECT trace_id::text FROM agent_traces WHERE trace_id=$1::uuid AND tenant_id=$2 AND trace_family=$3 AND ($4::text IS NULL OR user_id=$4)").bind(trace).bind(tenant).bind(family).bind(user).fetch_optional(conn).await})).await?;
        if exists.is_none() {
            return Ok(None);
        }
        let id = experiment_score_id(tenant_id, trace_id, trace_family, payload);
        let trace = trace_id.to_owned();
        let created_by = created_by.to_owned();
        let payload = payload.clone();
        if payload.get("score_name").is_none() {
            return Err(TraceError::invalid());
        }
        let result = self
            .run(move |conn| {
                Box::pin(async move {
                    let span = payload["span_id"].as_str();
                    let scorer = string_or(&payload, "scorer_type", "human");
                    let target_type = string_or(
                        &payload,
                        "target_type",
                        if span.is_some_and(|s| !s.is_empty()) {
                            "span"
                        } else {
                            "trace"
                        },
                    );
                    let target = string_or(
                        &payload,
                        "target_id",
                        span.filter(|s| !s.is_empty()).unwrap_or(&trace),
                    );
                    let source = string_or(&payload, "score_source", &scorer);
                    let metadata = payload
                        .get("metadata")
                        .filter(|v| truthy(v))
                        .cloned()
                        .unwrap_or(json!({}));
                    sqlx::query_scalar::<_, Value>(sql::CREATE_SCORE)
                        .bind(id)
                        .bind(trace)
                        .bind(span)
                        .bind(payload["score_name"].as_str())
                        .bind(string_or(&payload, "score_type", "numeric"))
                        .bind(payload["numeric_value"].as_f64())
                        .bind(payload["boolean_value"].as_bool())
                        .bind(payload["categorical_value"].as_str())
                        .bind(payload["text_value"].as_str())
                        .bind(payload["label"].as_str())
                        .bind(payload["explanation"].as_str())
                        .bind(scorer)
                        .bind(payload["evaluator_version"].as_str())
                        .bind(target_type)
                        .bind(target)
                        .bind(payload["evaluator_id"].as_str())
                        .bind(payload["evaluator_name"].as_str())
                        .bind(source)
                        .bind(payload["confidence"].as_f64())
                        .bind(created_by)
                        .bind(metadata)
                        .fetch_optional(conn)
                        .await
                })
            })
            .await?;
        Ok(result.map(|v| decode(v, "score")))
    }
    pub async fn create_eval_score(
        &self,
        tenant_id: &str,
        trace_id: &str,
        created_by: &str,
        payload: &Value,
        trace_family: &str,
    ) -> Result<Option<Value>, TraceError> {
        self.create_score(tenant_id, trace_id, created_by, payload, None, trace_family)
            .await
    }
    pub async fn create_eval_score_claimed(
        &self,
        tenant_id: &str,
        trace_id: &str,
        created_by: &str,
        payload: &Value,
        trace_family: &str,
        claim: &Value,
    ) -> Result<Option<Value>, TraceError> {
        self.with_outbox_claim(claim)?
            .create_eval_score(tenant_id, trace_id, created_by, payload, trace_family)
            .await
    }
}
