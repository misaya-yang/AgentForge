//! Authenticated recent calculated-context diagnostics. Production readers and
//! provider-bound producers share the same process or HA collector. Observations
//! are never reconstructed from billing rows or quota reservation estimates.
pub mod producer;
use crate::{
    migration_content::{ContentAccess, ContentError},
    sessions_api::SessionsApiManager,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextMetric {
    pub session_id: String,
    pub tenant_id: String,
    pub timestamp: DateTime<Utc>,
    pub total_tokens: i64,
    pub utilization: f64,
    pub compression_ratio: Option<f64>,
    pub cache_hit_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calculation: Option<Calculation>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Calculation {
    pub source: String,
    pub token_kind: String,
    pub context_window: i64,
    pub non_text_tokens_unknown: bool,
}
#[derive(Default)]
pub struct ContextMetricsCollector {
    recent: Mutex<VecDeque<ContextMetric>>,
    shared: Option<ai_platform_gateway_foundation::multi_instance::context::ContextStore>,
}
impl ContextMetricsCollector {
    pub fn shared(redis: redis::aio::ConnectionManager) -> Self {
        Self {
            recent: Mutex::new(VecDeque::new()),
            shared: Some(
                ai_platform_gateway_foundation::multi_instance::context::ContextStore::new(redis),
            ),
        }
    }
    pub fn shared_authority_configured(&self) -> bool {
        self.shared.is_some()
    }
    pub async fn record_shared(
        &self,
        event_id: &str,
        metric: ContextMetric,
    ) -> Result<bool, ai_platform_gateway_foundation::multi_instance::Error> {
        validate_metric(&metric)?;
        self.shared
            .as_ref()
            .ok_or(ai_platform_gateway_foundation::multi_instance::Error::Unavailable)?
            .record(&metric.tenant_id, &metric.session_id, event_id, &metric)
            .await
    }
    pub async fn observe(
        &self,
        event_id: &str,
        metric: ContextMetric,
    ) -> Result<bool, ai_platform_gateway_foundation::multi_instance::Error> {
        if self.shared.is_some() {
            self.record_shared(event_id, metric).await
        } else {
            self.record(metric)?;
            Ok(true)
        }
    }
    pub async fn read_session(
        &self,
        tenant: &str,
        session: &str,
        limit: usize,
    ) -> Result<Value, ai_platform_gateway_foundation::multi_instance::Error> {
        let collector = self.snapshot().await?;
        let mut recent = collector
            .recent
            .lock()
            .map_err(|_| ai_platform_gateway_foundation::multi_instance::Error::Unavailable)?;
        recent.retain(|metric| metric.tenant_id == tenant);
        drop(recent);
        Ok(collector.session(session, limit))
    }
    pub async fn read_tenant(
        &self,
        tenant: &str,
        hours: i64,
        now: DateTime<Utc>,
    ) -> Result<Value, ai_platform_gateway_foundation::multi_instance::Error> {
        Ok(self.snapshot().await?.tenant(tenant, hours, now))
    }
    async fn snapshot(
        &self,
    ) -> Result<Self, ai_platform_gateway_foundation::multi_instance::Error> {
        let recent = if let Some(store) = &self.shared {
            store.read::<ContextMetric>().await?.into_iter().collect()
        } else {
            self.recent
                .lock()
                .map_err(|_| ai_platform_gateway_foundation::multi_instance::Error::Unavailable)?
                .clone()
        };
        Ok(Self {
            recent: Mutex::new(recent),
            shared: None,
        })
    }
    pub fn global() -> Arc<Self> {
        static COLLECTOR: OnceLock<Arc<ContextMetricsCollector>> = OnceLock::new();
        COLLECTOR.get_or_init(|| Arc::new(Self::default())).clone()
    }
    /// The producer supplies calculated totals, like Python's calculate_totals.
    pub fn record(
        &self,
        metric: ContextMetric,
    ) -> Result<(), ai_platform_gateway_foundation::multi_instance::Error> {
        if self.shared.is_some() {
            return Err(ai_platform_gateway_foundation::multi_instance::Error::Unavailable);
        }
        validate_metric(&metric)?;
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() == 100 {
            recent.pop_front();
        }
        recent.push_back(metric);
        Ok(())
    }
    fn session(&self, session: &str, limit: usize) -> Value {
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let mut selected: Vec<_> = recent
            .iter()
            .rev()
            .filter(|m| m.session_id == session)
            .take(limit)
            .collect();
        selected.reverse();
        if selected.is_empty() {
            return json!({"session_id":session,"request_count":0,"avg_tokens":0,"avg_utilization":0.0,
                "avg_compression_ratio":0.0,"avg_cache_hit_rate":0.0,"total_tokens_used":null,"calculation":null});
        }
        let count = selected.len() as i64;
        let tokens: i64 = selected.iter().map(|m| m.total_tokens).sum();
        json!({"session_id":session,"request_count":count,"avg_tokens":tokens.div_euclid(count),
            "avg_utilization":rounded(selected.iter().map(|m| m.utilization).sum::<f64>() / count as f64,3),
            "avg_compression_ratio":known_average(selected.iter().filter_map(|m| m.compression_ratio),2),
            "avg_cache_hit_rate":known_average(selected.iter().filter_map(|m| m.cache_hit_rate),3),
            "total_tokens_used":tokens,
            "calculation":selected.last().and_then(|m|m.calculation.as_ref())})
    }
    fn tenant(&self, tenant: &str, hours: i64, now: DateTime<Utc>) -> Value {
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff = now - chrono::Duration::hours(hours);
        let selected: Vec<_> = recent
            .iter()
            .filter(|m| m.tenant_id == tenant && m.timestamp > cutoff)
            .collect();
        let tokens: i64 = selected.iter().map(|m| m.total_tokens).sum();
        let count = selected.len() as i64;
        let average = (count > 0).then(|| tokens.div_euclid(count));
        let utilization = if count > 0 {
            selected.iter().map(|m| m.utilization).sum::<f64>() / count as f64
        } else {
            0.0
        };
        json!({"tenant_id":tenant,"hours":hours,"request_count":count,
            "unique_sessions":selected.iter().map(|m|&m.session_id).collect::<BTreeSet<_>>().len(),
            "total_tokens":tokens,"avg_tokens_per_request":average,
            "avg_utilization":(utilization != 0.0).then(||rounded(utilization,3))})
    }
}
fn validate_metric(
    metric: &ContextMetric,
) -> Result<(), ai_platform_gateway_foundation::multi_instance::Error> {
    if metric.total_tokens < 0
        || metric.total_tokens > i64::MAX / 100
        || metric.tenant_id.is_empty()
        || metric.session_id.is_empty()
        || ![
            Some(metric.utilization),
            metric.compression_ratio,
            metric.cache_hit_rate,
        ]
        .into_iter()
        .flatten()
        .all(|v| v.is_finite() && (0.0..=1_000_000_000_000.0).contains(&v))
    {
        return Err(ai_platform_gateway_foundation::multi_instance::Error::Invalid);
    }
    Ok(())
}
fn known_average(values: impl Iterator<Item = f64>, digits: usize) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0usize), |(sum, count), v| (sum + v, count + 1));
    (count > 0).then(|| rounded(sum / count as f64, digits))
}
fn rounded(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}").parse().unwrap_or(value)
}
#[derive(Clone)]
pub struct ContextMetricsState {
    pub access: Arc<dyn ContentAccess>,
    pub sessions: Arc<dyn SessionsApiManager>,
    pub collector: Arc<ContextMetricsCollector>,
}
pub fn router(state: ContextMetricsState) -> Router {
    Router::new()
        .route(
            super::native_public_contract::SESSION_METRICS,
            get(session).head(method_not_allowed),
        )
        .route(
            super::native_public_contract::TENANT_METRICS,
            get(tenant).head(method_not_allowed),
        )
        .with_state(state)
}
async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [("allow", "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}
fn bounded_query(
    query: &HashMap<String, String>,
    name: &str,
    default: i64,
    max: i64,
) -> Result<i64, ContentError> {
    let Some(raw) = query.get(name) else {
        return Ok(default);
    };
    let raw_trimmed = raw.trim();
    let integer = raw_trimmed.parse::<i64>().ok().or_else(|| {
        let (whole, fraction) = raw_trimmed.split_once('.')?;
        if fraction.is_empty() || !fraction.bytes().all(|b| b == b'0') {
            return None;
        }
        whole.parse::<i64>().ok()
    });
    let Some(value) = integer else {
        return Err(ContentError::new(
            422,
            json!([{"type":"int_parsing","loc":["query",name],
            "msg":"Input should be a valid integer, unable to parse string as an integer","input":raw}]),
        ));
    };
    if value < 1 {
        return Err(ContentError::new(
            422,
            json!([{"type":"greater_than_equal","loc":["query",name],
            "msg":"Input should be greater than or equal to 1","input":raw,"ctx":{"ge":1}}]),
        ));
    }
    if value > max {
        return Err(ContentError::new(
            422,
            json!([{"type":"less_than_equal","loc":["query",name],
            "msg":format!("Input should be less than or equal to {max}"),"input":raw,"ctx":{"le":max}}]),
        ));
    }
    Ok(value)
}
async fn session(
    State(state): State<ContextMetricsState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let limit = bounded_query(&query, "limit", 50, 200)?;
    let user = state.access.resolve(&headers).await?;
    state
        .sessions
        .get(&id)
        .await
        .map_err(|_| ContentError::new(500, "Failed to verify session"))?
        .filter(|s| s.user_id == user.user_id && s.tenant_id == user.tenant_id)
        .ok_or_else(|| ContentError::new(404, "Session not found"))?;
    Ok(Json(
        state
            .collector
            .read_session(&user.tenant_id, &id, limit as usize)
            .await
            .map_err(|_| ContentError::new(503, "context_metrics_unavailable"))?,
    ))
}
async fn tenant(
    State(state): State<ContextMetricsState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let hours = bounded_query(&query, "hours", 24, 168)?;
    let user = state.access.resolve(&headers).await?;
    Ok(Json(
        state
            .collector
            .read_tenant(&user.tenant_id, hours, Utc::now())
            .await
            .map_err(|_| ContentError::new(503, "context_metrics_unavailable"))?,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn metric(id: &str, tenant: &str, tokens: i64, time: DateTime<Utc>) -> ContextMetric {
        ContextMetric {
            session_id: id.into(),
            tenant_id: tenant.into(),
            timestamp: time,
            total_tokens: tokens,
            utilization: 0.12345,
            compression_ratio: Some(1.125),
            cache_hit_rate: Some(0.56789),
            calculation: None,
        }
    }
    #[test]
    fn metric_query_bounds_match_python_integer_errors() {
        assert_eq!(
            bounded_query(&HashMap::new(), "limit", 50, 200).unwrap(),
            50
        );
        assert_eq!(
            bounded_query(
                &HashMap::from([("limit".into(), " 2.0 ".into())]),
                "limit",
                50,
                200
            )
            .unwrap(),
            2
        );
        for (raw, kind) in [
            ("oops", "int_parsing"),
            ("0", "greater_than_equal"),
            ("201", "less_than_equal"),
        ] {
            let error = bounded_query(
                &HashMap::from([("limit".into(), raw.into())]),
                "limit",
                50,
                200,
            )
            .unwrap_err();
            assert_eq!(error.status, 422);
            assert_eq!(error.detail[0]["type"], kind);
            assert_eq!(error.detail[0]["loc"], json!(["query", "limit"]));
        }
    }
    #[test]
    fn recent_window_limit_and_tenant_isolation() {
        let c = ContextMetricsCollector::default();
        let now = Utc::now();
        for i in 0..102 {
            c.record(metric("s", "a", i, now)).unwrap();
        }
        assert_eq!(c.session("s", 200)["request_count"], 100);
        assert_eq!(c.session("s", 2)["total_tokens_used"], 201);
        assert_eq!(c.session("s", 2)["avg_tokens"], 100);
        assert_eq!(c.session("s", 2)["avg_cache_hit_rate"], 0.568);
        assert_eq!(c.tenant("b", 24, now)["request_count"], 0);
        assert_eq!(c.session("missing", 50)["total_tokens_used"], Value::Null);
    }
    #[test]
    fn cutoff_is_strict_and_zero_utilization_is_null() {
        let c = ContextMetricsCollector::default();
        let now = Utc::now();
        c.record(metric("old", "a", 99, now - chrono::Duration::hours(24)))
            .unwrap();
        let mut m = metric("current", "a", 3, now);
        m.utilization = 0.0;
        c.record(m).unwrap();
        let v = c.tenant("a", 24, now);
        assert_eq!(v["request_count"], 1);
        assert_eq!(v["unique_sessions"], 1);
        assert_eq!(v["total_tokens"], 3);
        assert_eq!(v["avg_utilization"], Value::Null);
    }
    #[tokio::test]
    async fn identical_session_ids_do_not_mix_tenant_observations() {
        let collector = ContextMetricsCollector::default();
        let now = Utc::now();
        collector
            .record(metric("same", "tenant-a", 7, now))
            .unwrap();
        collector
            .record(metric("same", "tenant-b", 99, now))
            .unwrap();
        assert_eq!(
            collector
                .read_session("tenant-a", "same", 100)
                .await
                .unwrap()["total_tokens_used"],
            7
        );
        assert_eq!(
            collector
                .read_session("tenant-b", "same", 100)
                .await
                .unwrap()["total_tokens_used"],
            99
        );
    }
}
