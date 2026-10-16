//! Source freshness and tenant-scope assertions with an injected Redis command boundary.
use ai_platform_gateway_foundation::{
    metrics_recorder::{
        MetricCommand, MetricValue, MetricsFuture, MetricsRecorder, MetricsRedis, RecordOutcome,
        SystemMetricsClock,
    },
    realtime_metrics::{RealtimeMetricsService, RealtimeRedis, SystemRealtimeRuntime},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(Default)]
struct RedisFixture {
    calls: AtomicUsize,
    values: Mutex<BTreeMap<String, String>>,
    sets: Mutex<BTreeMap<String, BTreeMap<String, Value>>>,
}
impl RedisFixture {
    fn execute_commands(&self, commands: &[MetricCommand]) -> Vec<MetricValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut values = self.values.lock().unwrap();
        commands
            .iter()
            .map(|c| {
                let key = c.args[0].as_str().unwrap().to_owned();
                match c.op.as_str() {
                    "get" => values
                        .get(&key)
                        .cloned()
                        .map(MetricValue::Text)
                        .unwrap_or(MetricValue::Missing),
                    "incr" | "incrby" | "decr" => {
                        let delta = if c.op == "incrby" {
                            c.args[1].as_i64().unwrap()
                        } else if c.op == "decr" {
                            -1
                        } else {
                            1
                        };
                        let n = values
                            .get(&key)
                            .map(|v| v.parse::<i64>().unwrap())
                            .unwrap_or(0)
                            + delta;
                        values.insert(key, n.to_string());
                        MetricValue::Integer(n)
                    }
                    "set" => {
                        values.insert(key, c.args[1].as_str().unwrap().to_owned());
                        MetricValue::Integer(1)
                    }
                    "zadd" => {
                        self.sets
                            .lock()
                            .unwrap()
                            .entry(key)
                            .or_default()
                            .extend(c.args[1].as_object().unwrap().clone());
                        MetricValue::Integer(1)
                    }
                    "zremrangebyrank" => {
                        let mut sets = self.sets.lock().unwrap();
                        let set = sets.entry(key).or_default();
                        let mut ranked: Vec<_> = set
                            .iter()
                            .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
                            .collect();
                        ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                        let len = ranked.len() as i64;
                        let start = c.args[1].as_i64().unwrap();
                        let stop = c.args[2].as_i64().unwrap();
                        let start = if start < 0 { len + start } else { start }.max(0);
                        let stop = if stop < 0 { len + stop } else { stop }.min(len - 1);
                        for (i, (key, _)) in ranked.into_iter().enumerate() {
                            if (i as i64) >= start && (i as i64) <= stop {
                                set.remove(&key);
                            }
                        }
                        MetricValue::Integer(1)
                    }
                    "expire" => MetricValue::Integer(1),
                    op => panic!("unexpected production command {op}"),
                }
            })
            .collect()
    }
}
impl MetricsRedis for RedisFixture {
    fn execute<'a>(&'a self, c: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async move { Ok(self.execute_commands(c)) })
    }
    fn latency_samples(&self) -> MetricsFuture<'_, Vec<MetricValue>> {
        Box::pin(async {
            Ok(self
                .sets
                .lock()
                .unwrap()
                .get("metrics:latency:samples")
                .map(|set| set.keys().cloned().map(MetricValue::Text).collect())
                .unwrap_or_default())
        })
    }
}
impl RealtimeRedis for RedisFixture {
    fn execute<'a>(&'a self, c: &'a [MetricCommand]) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async move { Ok(self.execute_commands(c)) })
    }
    fn get<'a>(&'a self, k: &'a str) -> MetricsFuture<'a, MetricValue> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(k)
                .cloned()
                .map(MetricValue::Text)
                .unwrap_or(MetricValue::Missing))
        })
    }
    fn zrangebyscore<'a>(
        &'a self,
        _: &'a str,
        _: f64,
        _: f64,
        _: bool,
    ) -> MetricsFuture<'a, Vec<MetricValue>> {
        Box::pin(async { panic!("unexpected sorted read") })
    }
    fn zcount<'a>(&'a self, _: &'a str, _: f64, _: f64) -> MetricsFuture<'a, i64> {
        Box::pin(async { panic!("unexpected count") })
    }
    fn zcard<'a>(&'a self, _: &'a str) -> MetricsFuture<'a, i64> {
        Box::pin(async { panic!("unexpected cardinality") })
    }
    fn set_queue_depth(&self, _: i64) -> MetricsFuture<'_, ()> {
        Box::pin(async { panic!("unexpected queue write") })
    }
}
#[tokio::test]
async fn realtime_user_metrics_are_tenant_scoped() {
    let redis = Arc::new(RedisFixture::default());
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    for (tenant, input, output, threads) in [("tenant-a", 3, 5, 2), ("tenant-b", 101, 103, 9)] {
        let mut v = redis.values.lock().unwrap();
        v.insert(
            format!("metrics:tokens:tenant:{tenant}:user:same-user:input:{date}"),
            input.to_string(),
        );
        v.insert(
            format!("metrics:tokens:tenant:{tenant}:user:same-user:output:{date}"),
            output.to_string(),
        );
        v.insert(
            format!("metrics:rt:threads:tenant:{tenant}:user:same-user"),
            threads.to_string(),
        );
    }
    let service = RealtimeMetricsService::new(Some(redis), Arc::new(SystemRealtimeRuntime));
    let result = service.get_user_metrics("same-user", "tenant-a").await;
    assert_eq!(
        result["tokens"],
        serde_json::json!({"input":3,"output":5,"total":8})
    );
    assert_eq!(result["active_threads"], 2);
}
#[tokio::test]
async fn realtime_request_threads_are_recorded_under_tenant_key() {
    let redis = Arc::new(RedisFixture::default());
    let service = RealtimeMetricsService::new(Some(redis.clone()), Arc::new(SystemRealtimeRuntime));
    assert_eq!(
        service
            .record_request_start("req-1", "same-user", "tenant-a")
            .await,
        RecordOutcome::Recorded
    );
    let v = redis.values.lock().unwrap();
    assert_eq!(v["metrics:rt:threads:tenant:tenant-a:user:same-user"], "1");
    assert!(!v.contains_key("metrics:rt:threads:same-user"));
    assert!(
        redis.sets.lock().unwrap()["metrics:rt:active_users"].contains_key("tenant-a:same-user")
    );
}
#[tokio::test]
// Original test expects empty, but its own production implementation returns unavailable.
// Preserve production behavior and record the original assertion as a retirement residual.
async fn absent_redis_preserves_source_production_unavailable_status() {
    let result = MetricsRecorder::default().get_today_summary().await;
    assert_eq!(result["data_status"], "unavailable");
    assert_eq!(result["data_source"], "none");
    assert!(result["last_ingested_at"].is_null());
}
#[tokio::test]
async fn redis_metrics_report_fresh_ingestion_timestamp() {
    let redis = Arc::new(RedisFixture::default());
    let recorder = MetricsRecorder::new(Some(redis), Arc::new(SystemMetricsClock), 10000);
    assert_eq!(
        recorder
            .record_request(
                "GET",
                "/api/v1/proxy/local/assistants/search",
                200,
                42.0,
                "user-a",
                "local"
            )
            .await,
        RecordOutcome::Recorded
    );
    let result = recorder.get_today_summary().await;
    assert_eq!(result["total_requests"], 1);
    assert_eq!(result["data_status"], "ok");
    assert_eq!(result["data_source"], "redis");
    assert!(result["data_freshness_minutes"].as_f64().unwrap() <= 1.0);
    assert!(
        chrono::DateTime::parse_from_rfc3339(result["last_ingested_at"].as_str().unwrap()).is_ok()
    );
}
#[tokio::test]
async fn token_metrics_use_tenant_scoped_user_keys() {
    let redis = Arc::new(RedisFixture::default());
    let recorder = MetricsRecorder::new(Some(redis.clone()), Arc::new(SystemMetricsClock), 10000);
    assert_eq!(
        recorder
            .record_tokens("same-user", "assistant", 7, 11, "", "tenant-a")
            .await,
        RecordOutcome::Recorded
    );
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let v = redis.values.lock().unwrap();
    for (kind, n) in [("input", "7"), ("output", "11")] {
        assert_eq!(
            v[&format!("metrics:tokens:tenant:tenant-a:user:same-user:{kind}:{date}")],
            n
        );
        assert!(!v.contains_key(&format!("metrics:tokens:user:same-user:{kind}:{date}")));
    }
}
#[test]
fn dlq_and_consumer_lag_alerts_are_configured() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let data: Value = serde_yaml::from_str(
        &std::fs::read_to_string(root.join("docker/monitoring/alert-rules.yml")).unwrap(),
    )
    .unwrap();
    let alerts: Vec<_> = data["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["rules"].as_array().unwrap())
        .filter_map(|r| r["alert"].as_str())
        .collect();
    for name in ["HighDLQDepth", "CriticalDLQDepth", "EventConsumerLag"] {
        assert!(alerts.contains(&name), "{name}");
    }
}
#[tokio::test]
async fn realtime_token_recording_is_a_single_writer_noop_with_or_without_redis() {
    let redis = Arc::new(RedisFixture::default());
    let service = RealtimeMetricsService::new(Some(redis.clone()), Arc::new(SystemRealtimeRuntime));
    let result: () = service.record_token_usage(1000, 500).await;
    assert_eq!(result, ());
    let result: () = service.record_token_usage(123, 456).await;
    assert_eq!(result, ());
    assert_eq!(redis.calls.load(Ordering::SeqCst), 0);
    assert!(redis.values.lock().unwrap().is_empty());
    assert!(redis.sets.lock().unwrap().is_empty());
    RealtimeMetricsService::new(None, Arc::new(SystemRealtimeRuntime))
        .record_token_usage(10, 20)
        .await;
}
async fn record_concurrently(recorder: MetricsRecorder, count: usize, identical: bool) {
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..count {
        let recorder = recorder.clone();
        tasks.spawn(async move {
            recorder
                .record_request(
                    "GET",
                    "/x",
                    200,
                    if identical { 100.0 } else { (i + 1) as f64 },
                    "",
                    "",
                )
                .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), RecordOutcome::Recorded);
    }
}
#[tokio::test]
async fn no_collision_under_one_thousand_concurrent_identical_latency_samples() {
    let redis = Arc::new(RedisFixture::default());
    let recorder = MetricsRecorder::new(Some(redis.clone()), Arc::new(SystemMetricsClock), 10000);
    record_concurrently(recorder, 1000, true).await;
    assert_eq!(
        redis.sets.lock().unwrap()["metrics:latency:samples"].len(),
        1000
    );
}
#[tokio::test]
async fn percentile_parser_extracts_latency_from_current_member_format() {
    let redis = Arc::new(RedisFixture::default());
    let recorder = MetricsRecorder::new(Some(redis), Arc::new(SystemMetricsClock), 10000);
    record_concurrently(recorder.clone(), 100, false).await;
    let p = recorder.get_latency_percentiles().await;
    for key in ["p50", "p95", "p99"] {
        assert!(p[key].as_f64().unwrap() > 0.0);
    }
    assert!(p["p99"].as_f64().unwrap() >= 90.0);
}
#[tokio::test]
async fn latency_sample_cap_honours_setting() {
    let redis = Arc::new(RedisFixture::default());
    let recorder = MetricsRecorder::new(Some(redis.clone()), Arc::new(SystemMetricsClock), 50);
    record_concurrently(recorder, 120, false).await;
    assert_eq!(
        redis.sets.lock().unwrap()["metrics:latency:samples"].len(),
        50
    );
}
#[tokio::test]
async fn metrics_endpoint_requires_metrics_read() {
    use ai_platform_gateway_foundation::metrics_admin_api::*;
    use axum::{
        body::{Body, to_bytes},
        http::{HeaderMap, Request, StatusCode},
        response::{IntoResponse, Response},
    };
    use tower::ServiceExt;
    struct Identity;
    impl MetricsAdminIdentityResolver for Identity {
        fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> MetricsAdminAuthFuture<'a> {
            Box::pin(async {
                Ok(MetricsAdminIdentity::from_resolved_identity(
                    "user-a".into(),
                    "tenant-a".into(),
                    vec!["user".into()],
                    vec![],
                    true,
                ))
            })
        }
    }
    struct Cancellation;
    impl MetricsAdminCancellation for Cancellation {
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send + '_>> {
            Box::pin(async { StatusCode::SERVICE_UNAVAILABLE.into_response() })
        }
    }
    let api = MetricsAdminApi::new(
        Arc::new(MetricsRecorder::default()),
        None,
        None,
        Arc::new(SystemMetricsAdminClock),
        ai_platform_auth::rbac::Rbac::default(),
        None,
    );
    let response = router(Arc::new(Identity), Arc::new(api), Arc::new(Cancellation))
        .oneshot(
            Request::builder()
                .uri("/api/v1/metrics/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("GatewayMetricsRead"));
}
