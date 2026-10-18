//! Concrete sampled request-trace writer used by the accepted usage recorder.
use super::truthy;
use crate::usage_recorder::{UsageFuture, UsageRecord, UsageTraceWriter};
use sqlx::PgPool;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
#[derive(Clone)]
pub struct SampledUsageTraceWriter {
    pub normal_sample_rate: f64,
    pub cache_ttl: Duration,
    pub default_p95_ms: i64,
    cache: Arc<Mutex<BTreeMap<String, (i64, std::time::SystemTime)>>>,
    random: Arc<dyn Fn() -> f64 + Send + Sync>,
}
impl Default for SampledUsageTraceWriter {
    fn default() -> Self {
        Self {
            normal_sample_rate: 0.03,
            cache_ttl: Duration::from_secs(60),
            default_p95_ms: 10000,
            cache: Default::default(),
            random: Arc::new(|| {
                let bits = uuid::Uuid::new_v4().as_u128() as u64 & ((1u64 << 53) - 1);
                bits as f64 / (1u64 << 53) as f64
            }),
        }
    }
}
impl SampledUsageTraceWriter {
    pub fn with_random(mut self, random: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        self.random = random;
        self
    }
    async fn threshold(&self, pool: &PgPool, tenant: &str) -> i64 {
        let key = if tenant.is_empty() { "default" } else { tenant };
        let now = std::time::SystemTime::now();
        if let Some((value, stamp)) = self.cache.lock().await.get(key).copied() {
            if now
                .duration_since(stamp)
                .map_or(true, |v| v <= self.cache_ttl.max(Duration::from_secs(1)))
            {
                return value;
            }
        }
        let value=sqlx::query_scalar::<_,f64>("SELECT p95_latency_ms::float8 FROM usage_daily_aggregates WHERE tenant_id=$1 AND p95_latency_ms>0 ORDER BY date DESC,p95_latency_ms DESC LIMIT 1").bind(tenant).fetch_optional(pool).await.ok().flatten().map(|v|(v as i64).max(1)).unwrap_or(self.default_p95_ms.max(1));
        self.cache.lock().await.insert(key.into(), (value, now));
        value
    }
}
fn normalized<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() { fallback } else { value }
}
pub fn should_sample_trace(
    status: &str,
    duration: i64,
    p95: i64,
    rate: f64,
    random: f64,
) -> (bool, &'static str) {
    if status.trim().to_lowercase() != "success" {
        return (true, "failure");
    }
    if p95 > 0 && duration.max(0) >= p95 {
        return (true, "slow_request");
    }
    if rate.clamp(0., 1.) <= 0. {
        return (false, "not_sampled");
    }
    (random < rate.clamp(0., 1.), "baseline")
}
impl UsageTraceWriter for SampledUsageTraceWriter {
    fn write<'a>(&'a self, pool: &'a PgPool, records: &'a [UsageRecord]) -> UsageFuture<'a, ()> {
        Box::pin(async move {
            let mut thresholds = BTreeMap::new();
            let mut selected = Vec::new();
            for r in records {
                if r.metadata.get("_accounting_adjustment").is_some_and(truthy) {
                    continue;
                }
                let tenant = normalized(&r.tenant_id, "default");
                let threshold = if let Some(v) = thresholds.get(tenant) {
                    *v
                } else {
                    let v = self.threshold(pool, tenant).await;
                    thresholds.insert(tenant.to_owned(), v);
                    v
                };
                let duration = if r.request_total_duration_ms > 0 {
                    r.request_total_duration_ms
                } else {
                    r.latency_ms.max(0)
                };
                let (sampled, reason) = should_sample_trace(
                    &r.status,
                    duration,
                    threshold,
                    self.normal_sample_rate,
                    (self.random)(),
                );
                if sampled {
                    selected.push((r, tenant, duration, reason));
                }
            }
            if selected.is_empty() {
                return Ok(());
            }
            let mut tx = pool.begin().await?;
            for (r, tenant, duration, reason) in &selected {
                ai_platform_service_primitives::data_connection::append_store::bind_tenant(
                    &mut tx,
                    "gateway.request_traces",
                    tenant,
                )
                .await?;
                sqlx::query("INSERT INTO request_traces(request_id,tenant_id,user_id,service_id,assistant_id,provider,model,status,error_type,request_total_duration_ms,first_token_latency_ms,llm_inference_duration_ms,retrieval_duration_ms,tool_call_duration_ms,agent_or_graph_overhead_ms,input_tokens,output_tokens,total_cost_cents,sample_reason,trace_steps,metadata,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20::jsonb,$21::jsonb,$22)").bind(&r.request_id).bind(*tenant).bind(normalized(&r.user_id,"anonymous")).bind(normalized(&r.service_id,"unattributed_service")).bind(normalized(&r.assistant_id,"")).bind(normalized(&r.provider,"unattributed")).bind(normalized(&r.model,"unattributed_model")).bind(if r.status.is_empty(){"success"}else{&r.status}).bind(&r.error_type).bind(*duration).bind(r.first_token_ms.max(0)).bind(r.llm_inference_duration_ms.max(0)).bind(r.retrieval_duration_ms.max(0)).bind(r.tool_call_duration_ms.max(0)).bind(r.agent_or_graph_overhead_ms.max(0)).bind(r.input_tokens.max(0)).bind(r.output_tokens.max(0)).bind(r.input_cost_cents.saturating_add(r.output_cost_cents).max(0)).bind(*reason).bind(serde_json::json!(r.trace_steps)).bind(serde_json::json!(r.metadata)).bind(r.timestamp).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            let mut tx = pool.begin().await?;
            for (r, tenant, _, reason) in selected {
                ai_platform_service_primitives::data_connection::append_store::bind_tenant(
                    &mut tx,
                    "gateway.usage_records",
                    tenant,
                )
                .await?;
                sqlx::query("UPDATE usage_records SET trace_sampled=TRUE,sample_reason=$3 WHERE request_id=$1 AND tenant_id=$2").bind(&r.request_id).bind(tenant).bind(reason).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(())
        })
    }
}
