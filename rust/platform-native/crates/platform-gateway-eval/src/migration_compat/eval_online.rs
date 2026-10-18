//! Source-equivalent online sampling, shared by the claimed worker and contract tests.
use super::common::CompatError;
use ai_platform_gateway_foundation::python_value::python_str;
use serde_json::{Value, json};
pub trait OnlineRepository: Sync {
    fn detail(
        &self,
        tenant: &str,
        trace: &str,
        family: &str,
    ) -> impl std::future::Future<Output = Result<Value, CompatError>> + Send;
    fn pending(
        &self,
        tenant: &str,
    ) -> impl std::future::Future<Output = Result<i64, CompatError>> + Send;
    fn evaluators(
        &self,
        tenant: &str,
        limit: i64,
        offset: i64,
    ) -> impl std::future::Future<Output = Result<(Vec<Value>, i64), CompatError>> + Send;
    fn active(
        &self,
        tenant: &str,
        evaluator: &str,
        trace: &str,
    ) -> impl std::future::Future<Output = Result<bool, CompatError>> + Send;
    fn enqueue(
        &self,
        tenant: &str,
        evaluator: &str,
        creator: &str,
        payload: Value,
    ) -> impl std::future::Future<Output = Result<(), CompatError>> + Send;
}
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_bool().map(|v| if v { 1.0 } else { 0.0 }))
        .or_else(|| v.as_str()?.trim().parse().ok())
}
pub fn resolve_rate(e: &Value, default: f64) -> f64 {
    match e["sampling_config"]["online"].get("rate") {
        None => default,
        Some(v) if !truthy(v) => 0.0,
        Some(v) => number(v).unwrap_or(default),
    }
}
pub fn resolve_cap(e: &Value, default: i64) -> i64 {
    match e["sampling_config"]["online"].get("max_pending_runs") {
        None => default,
        Some(v) if !truthy(v) => default.max(1),
        Some(v) => {
            let n = if let Some(s) = v.as_str() {
                s.trim().parse::<i64>().ok()
            } else {
                number(v).filter(|v| v.is_finite()).map(|v| v as i64)
            };
            n.map(|v| v.max(1)).unwrap_or(default)
        }
    }
}
pub fn matches_online(e: &Value, family: &str, status: &str) -> bool {
    let c = &e["sampling_config"]["online"];
    if !truthy(&c["enabled"])
        || !truthy(&e["evaluator_type"])
        || python_str(&e["evaluator_type"]) == "human"
    {
        return false;
    }
    let allowed = match c["trace_families"].as_array().filter(|v| !v.is_empty()) {
        Some(v) => v.iter().any(|v| python_str(v) == family),
        None => matches!(family, "assistant" | "langgraph_proxy" | "rag"),
    };
    allowed
        && (!truthy(&c["only_failed"])
            || matches!(status, "failed" | "cancelled" | "timeout" | "error"))
        && (!truthy(&c["only_succeeded"]) || status == "succeeded")
}
pub struct Options<'a> {
    pub creator: &'a str,
    pub rate: f64,
    pub limit: i64,
    pub cap: i64,
}
impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            creator: "eval-online-sampler",
            rate: 0.05,
            limit: 100,
            cap: 200,
        }
    }
}
pub async fn schedule(
    repo: &impl OnlineRepository,
    tenant: &str,
    payload: &Value,
    options: Options<'_>,
) -> Result<Value, CompatError> {
    let trace_string = if truthy(&payload["trace_id"]) {
        python_str(&payload["trace_id"])
    } else {
        String::new()
    };
    let trace = trace_string.trim();
    if trace.is_empty() {
        return Ok(json!({"scheduled":0,"reason":"missing_trace_id"}));
    }
    let raw_family = if truthy(&payload["trace_family"]) {
        python_str(&payload["trace_family"])
    } else {
        "assistant".into()
    };
    let detail = repo.detail(tenant, trace, &raw_family).await?;
    let origin = [
        &payload["eval_origin"],
        &detail["trace"]["metadata"]["eval_origin"],
        &detail["trace"]["user_id"],
    ]
    .into_iter()
    .find(|v| truthy(v))
    .map(python_str)
    .unwrap_or_default();
    let origin = origin.trim();
    if matches!(
        origin,
        "candidate" | "judge" | "eval-candidate" | "eval-worker"
    ) {
        return Ok(
            json!({"scheduled":0,"matched":0,"trace_id":trace,"reason":"internal_eval_trace"}),
        );
    }
    let pending = repo.pending(tenant).await?;
    if pending >= options.cap {
        return Ok(
            json!({"scheduled":0,"matched":0,"trace_id":trace,"reason":"online_eval_queue_full","pending_online_runs":pending}),
        );
    }
    let family = match raw_family.trim() {
        v @ ("assistant" | "langgraph_proxy" | "rag") => v,
        _ => "assistant",
    };
    let status = if truthy(&payload["status"]) {
        python_str(&payload["status"]).trim().to_lowercase()
    } else {
        "succeeded".into()
    };
    let (mut all, mut offset) = (Vec::new(), 0);
    loop {
        let (page, total) = repo
            .evaluators(tenant, options.limit.max(1), offset)
            .await?;
        let empty = page.is_empty();
        offset += page.len() as i64;
        all.extend(page);
        if empty || offset >= total {
            break;
        }
    }
    let (mut scheduled, mut matched) = (0, 0);
    for e in all {
        if !matches_online(&e, family, &status) {
            continue;
        }
        matched += 1;
        if !super::eval_executor::should_sample_trace_id(trace, resolve_rate(&e, options.rate)) {
            continue;
        }
        let id = if truthy(&e["evaluator_id"]) {
            python_str(&e["evaluator_id"])
        } else {
            String::new()
        };
        if id.is_empty() || repo.active(tenant, &id, trace).await? {
            continue;
        }
        if pending + scheduled >= resolve_cap(&e, options.cap) {
            continue;
        }
        repo.enqueue(tenant,&id,options.creator,json!({"trace_id":trace,"target_snapshot":{"trace_id":trace,"trace_family":family,"source":"online_sampling","status":status,"source_adapter":payload["source_adapter"]},"metadata":{"online_sampling":true}})).await?;
        scheduled += 1;
    }
    Ok(json!({"scheduled":scheduled,"matched":matched,"trace_id":trace}))
}
