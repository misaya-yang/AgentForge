//! Source capacity resolver and hot-path Redis round-trip assertions.
use ai_platform_gateway_foundation::{
    capacity::{CapacityRequest, CapacityResolver},
    rate_limits::*,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[test]
fn capacity_defaults_overrides_admin_bypass_and_missing_budget_contracts() {
    let resolver = CapacityResolver::default();
    let mut request:CapacityRequest=serde_json::from_value(json!({"tenant_id":"tenant-a","service_id":"local-2024-agent","request_class":"stream","upstream_group":null,"provider_id":"google"})).unwrap();
    let budgets = resolver.resolve(&request).unwrap();
    let by_key = budgets
        .iter()
        .map(|b| (b.key.as_str(), b))
        .collect::<BTreeMap<_, _>>();
    for (key, limit) in [
        ("gateway.total_inflight", 64),
        ("gateway.stream_inflight", 16),
        ("upstream.langgraph_agent", 4),
        ("provider.google_gemini", 4),
    ] {
        assert_eq!(by_key[key].limit, limit);
    }
    assert!(budgets.iter().all(|b| b.enforced));
    request.request_class = "sync".into();
    request.provider_id = serde_json::Value::Null;
    request.service_config = json!({"capacity":{"upstream_group":"langgraph_agent","concurrency_limit":3,"queue_max":0,"queue_timeout_ms":100}});
    let budgets = resolver.resolve(&request).unwrap();
    let upstream = budgets
        .iter()
        .find(|b| b.key == "upstream.langgraph_agent")
        .unwrap();
    assert_eq!(upstream.limit, 3);
    assert_eq!(upstream.queue_max, 0);
    assert_eq!(upstream.queue_timeout_ms, 100);
    assert_eq!(upstream.source, "service_config");
    request.service_config = serde_json::Value::Null;
    request.is_admin_read = true;
    request.request_class = "admin".into();
    let budgets = resolver.resolve(&request).unwrap();
    assert!(budgets.iter().any(|b| b.key == "gateway.total_inflight"));
    assert!(!budgets.iter().any(|b| b.key == "upstream.langgraph_agent"));
    request.is_admin_read = false;
    request.request_class = "sync".into();
    request.service_id = "unmapped-service".into();
    let budgets = resolver.resolve(&request).unwrap();
    let missing = budgets
        .iter()
        .filter(|b| b.source_status == "missing")
        .collect::<Vec<_>>();
    assert!(!missing.is_empty());
    assert!(missing.iter().all(|b| !b.enforced));
}
struct RedisFixture {
    counts: BTreeMap<String, i64>,
    calls: AtomicU64,
}
impl SlidingWindowStore for RedisFixture {
    fn check<'a>(&'a self, batch: &'a SlidingWindowBatch) -> SlidingWindowFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            for (index, p) in batch.policies.iter().enumerate() {
                if self.counts.get(&p.key).copied().unwrap_or(0) >= p.requests {
                    return Ok(vec![index as f64, batch.window_start_seconds + 30.0]);
                }
            }
            Ok(vec![-1.0, 0.0])
        })
    }
}
#[tokio::test]
async fn successful_redis_checks_increment_global_hot_path_once_per_atomic_round_trip() {
    // Only this test in this integration-test process mutates the global counter.
    for (denied, multi, expected_calls) in [
        (false, false, 1),
        (true, false, 1),
        (false, true, 2),
        (true, true, 1),
    ] {
        GATEWAY_HOT_PATH_METRICS.reset();
        let key = if multi {
            "ratelimit:global"
        } else {
            "ratelimit:user:u1"
        };
        let store = Arc::new(RedisFixture {
            counts: if denied {
                BTreeMap::from([(key.into(), 1001)])
            } else {
                BTreeMap::new()
            },
            calls: AtomicU64::new(0),
        });
        let limiter = SlidingWindowRateLimiter::new(Some(store.clone()));
        let policies = if multi {
            let config = MultiDimensionConfig {
                ip: None,
                tenant: None,
                assistant: None,
                global: Some(TierLimit::new(1000, 60)),
                ..Default::default()
            };
            config
                .policies(
                    &RateLimitContext {
                        user_id: Some("user-a".into()),
                        user_tier: "normal".into(),
                        ..Default::default()
                    },
                    &Default::default(),
                )
                .unwrap()
        } else {
            vec![RatePolicy::new(key, "user", 10, 60)]
        };
        let result = limiter.check_dimensions_at(&policies, 100.0).await.unwrap();
        assert_eq!(result.allowed, !denied);
        assert_eq!(store.calls.load(Ordering::SeqCst), expected_calls);
        assert_eq!(GATEWAY_HOT_PATH_METRICS.redis_round_trips(), expected_calls);
        if denied && !multi {
            assert_eq!(result.retry_after, 31);
        }
    }
    GATEWAY_HOT_PATH_METRICS.reset();
}
