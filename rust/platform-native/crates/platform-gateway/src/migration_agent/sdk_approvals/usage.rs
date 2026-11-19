//! Bounded, read-only observations for an already owner-authenticated SDK run.
use ai_platform_gateway_foundation::sdk_capabilities::canonical_hash;
use serde_json::{Value, json};
use sqlx::PgPool;
const INDEX_READY: &str = "SELECT EXISTS (SELECT 1 FROM pg_index WHERE indexrelid=to_regclass('gateway.sdk_usage_audit_lookup') AND indrelid='gateway.audit_logs'::regclass AND indisvalid AND indisready)";
const SELECT: &str = "SELECT request_summary,response_summary FROM gateway.audit_logs WHERE event_type='sdk_usage_observation' AND resource_type='sdk_request' AND resource_id=$1 AND tenant_id=$2 AND user_id=$3 AND action=$4 ORDER BY id LIMIT 2";
fn unavailable(reason: &str) -> Value {
    json!({"sdk_usage":{"state":"unavailable","cost_state":"unavailable","provider_outcome":"unknown","platform_pricing_state":"reconciliation_required","accounting_status":"not_asserted","audit_persistence":reason}})
}
fn tokens(value: &Value) -> Value {
    let mut result = json!({});
    for field in ["input_tokens", "output_tokens", "total_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "cached_tokens", "cache_write_tokens"] {
        if let Some(v) = value.get(field).filter(|v| v.as_i64().is_some_and(|n| n >= 0)) { result[field] = v.clone(); }
    }
    if let Some(details) = value.get("input_tokens_details").filter(|v| v.is_object()) {
        let mut filtered = json!({});
        for field in ["cached_tokens", "cache_write_tokens"] {
            if let Some(v) = details.get(field).filter(|v| v.as_i64().is_some_and(|n| n >= 0)) { filtered[field] = v.clone(); }
        }
        result["input_tokens_details"] = filtered;
    }
    result
}
fn complete_tokens(observation: &Value) -> bool {
    let Some(input) = observation["input_tokens"].as_i64().filter(|n| *n >= 0) else { return false; };
    let Some(output) = observation["output_tokens"].as_i64().filter(|n| *n >= 0) else { return false; };
    let Some(total) = input.checked_add(output) else { return false; };
    observation.get("total_tokens").is_none_or(|value| value.as_i64() == Some(total))
}
fn project(observation: &Value) -> Value {
    let mut usage = tokens(observation);
    let mut metadata = unavailable("durable")["sdk_usage"].clone();
    if observation["usage_state"] == "reported" && complete_tokens(observation) { metadata["state"] = json!("reported"); }
    if observation["provider_outcome"] == "completed" { metadata["provider_outcome"] = json!("completed"); }
    if observation["platform_pricing_state"] == "two_rate_compatible" { metadata["platform_pricing_state"] = json!("two_rate_compatible"); }
    if let Some(cost) = observation.get("provider_cost_usd").filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)) {
        metadata["provider_cost_usd"] = cost.clone(); metadata["cost_state"] = json!("reported");
    }
    if let Some(raw) = observation.get("provider_usage") {
        if raw.is_object() { metadata["provider_usage"] = tokens(raw); }
        else if let Some(items) = raw.as_array().filter(|items| items.len() <= 32) { metadata["provider_usage"] = Value::Array(items.iter().map(tokens).collect()); }
    }
    usage["sdk_usage"] = metadata;
    usage
}
fn bound(summary: &Value, identity: &Value, run: &str) -> bool {
    summary["schema"] == "sdk-usage-audit/v1" && summary["assistant_run_id"] == run
        && identity.as_object().is_some_and(|fields| fields.iter().all(|(key, value)| summary.get(key) == Some(value)))
}
fn reconcile(identity: &Value, run: &str, phases: &[Vec<(Value, Value)>]) -> Value {
    let Some(admission) = phases.first().filter(|rows| rows.len() == 1).map(|rows| &rows[0].0) else { return unavailable("unresolved_admission"); };
    if !bound(admission, identity, run) || phases.iter().any(|rows| rows.len() > 1) { return unavailable("identity_conflict"); }
    let terminals: Vec<_> = phases.iter().enumerate().skip(1)
        .flat_map(|(phase, rows)| rows.iter().map(move |row| (phase, row))).collect();
    if terminals.is_empty() { return unavailable("unresolved_admission"); }
    if terminals.len() != 1 { return unavailable("identity_conflict"); }
    let (phase, (summary, observation)) = terminals[0];
    let expected = ["admitted", "completed", "cancelled", "failed"].get(phase).copied();
    if summary != admission || expected.is_none() || observation["local_execution_status"].as_str() != expected {
        return unavailable("identity_conflict");
    }
    project(observation)
}
// One budget covers pool acquisition, transaction setup, index readiness and all
// phase reads. PostgreSQL limits also release a locked query after client drop.
async fn bounded(observation: impl std::future::Future<Output = Value>) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(1), observation)
        .await.unwrap_or_else(|_| unavailable("unavailable"))
}
pub(super) async fn read(pool: &PgPool, identity: &Value, run: &str) -> Value {
    bounded(observe(pool, identity, run)).await
}
async fn observe(pool: &PgPool, identity: &Value, run: &str) -> Value {
    let Ok(mut tx) = pool.begin().await else { return unavailable("unavailable"); };
    if sqlx::query("SELECT set_config('statement_timeout','250ms',true),set_config('lock_timeout','100ms',true)")
        .execute(&mut *tx).await.is_err() { return unavailable("unavailable"); }
    // Older installations must not scan a growing audit history without epoch16.
    if !matches!(sqlx::query_scalar::<_, bool>(INDEX_READY).fetch_one(&mut *tx).await, Ok(true)) { return unavailable("unavailable"); }
    let resource = canonical_hash(identity);
    let mut phases = Vec::new();
    for phase in ["admitted", "completed", "cancelled", "failed"] {
        let rows = sqlx::query_as::<_, (Value, Value)>(SELECT).bind(&resource)
            .bind(identity["tenant_id"].as_str().unwrap_or(""))
            .bind(identity["user_id"].as_str().unwrap_or(""))
            .bind(phase).fetch_all(&mut *tx).await;
        let Ok(rows) = rows else { return unavailable("unavailable"); };
        phases.push(rows);
    }
    let result = reconcile(identity, run, &phases);
    if tx.rollback().await.is_err() { return unavailable("unavailable"); }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> Value { json!({"tenant_id":"t","user_id":"u","service_id":"s","request_id":"q","session_id":"session","framework":"openai_agents"}) }
    fn summary() -> Value { let mut v=identity();v["schema"]=json!("sdk-usage-audit/v1");v["assistant_run_id"]=json!("run");v }
    #[tokio::test]
    async fn blocked_optional_lookup_preserves_unavailable_usage() {
        let result = bounded(std::future::pending::<Value>()).await;
        assert_eq!(result["sdk_usage"]["state"], "unavailable");
        assert_eq!(result["sdk_usage"]["accounting_status"], "not_asserted");
        assert!(result.get("input_tokens").is_none());
    }
    #[test]
    fn missing_conflicting_and_wrong_run_observations_never_invent_tokens() {
        let admission=vec![(summary(),json!({}))];
        let duplicate=vec![(summary(),json!({})),(summary(),json!({}))];
        for phases in [vec![vec![]],vec![admission.clone()],vec![admission.clone(),duplicate],vec![admission.clone(),vec![(summary(),json!({}))],vec![(summary(),json!({}))]]] {
            let v=reconcile(&identity(),"run",&phases);
            assert_eq!(v["sdk_usage"]["state"],"unavailable");assert!(v.get("input_tokens").is_none());
        }
        assert_eq!(reconcile(&identity(),"other",&[admission])["sdk_usage"]["audit_persistence"],"identity_conflict");
    }
    #[test]
    fn reported_requires_complete_consistent_nonnegative_tokens() {
        for counts in [json!({}), json!({"input_tokens":-1,"output_tokens":2}),
            json!({"input_tokens":"3","output_tokens":2}),
            json!({"input_tokens":3,"output_tokens":2,"total_tokens":6}),
            json!({"input_tokens":i64::MAX,"output_tokens":1})] {
            let mut observation=counts;observation["usage_state"]=json!("reported");
            observation["provider_cost_usd"]=json!(0.25);
            let result=project(&observation);
            assert_eq!(result["sdk_usage"]["state"],"unavailable");
            assert_eq!(result["sdk_usage"]["provider_cost_usd"],0.25);
        }
        assert_eq!(project(&json!({"usage_state":"reported","input_tokens":3,"output_tokens":2,"total_tokens":5}))["sdk_usage"]["state"],"reported");
    }
    #[test]
    fn mismatched_terminal_phase_is_unavailable() {
        let result=reconcile(&identity(),"run",&[vec![(summary(),json!({}))],vec![(summary(),json!({"local_execution_status":"cancelled","usage_state":"reported","input_tokens":1,"output_tokens":1}))]]);
        assert_eq!(result["sdk_usage"]["audit_persistence"],"identity_conflict");
        assert!(result.get("input_tokens").is_none());
    }
    #[test]
    fn partial_cache_and_cost_are_observations_not_charges() {
        let observation=json!({"local_execution_status":"completed","output_tokens":2,"usage_state":"unavailable","provider_cost_usd":0.25,"provider_usage":{"cache_read_input_tokens":3,"secret":"hidden"},"accounting_status":"settled","secret":"hidden"});
        let v=reconcile(&identity(),"run",&[vec![(summary(),json!({}))],vec![(summary(),observation)]]);
        assert_eq!(v["output_tokens"],2);assert!(v.get("input_tokens").is_none());
        assert_eq!(v["sdk_usage"]["provider_cost_usd"],0.25);assert_eq!(v["sdk_usage"]["provider_usage"]["cache_read_input_tokens"],3);
        assert_eq!(v["sdk_usage"]["accounting_status"],"not_asserted");assert!(!v.to_string().contains("hidden"));
    }
}
