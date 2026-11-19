//! Source tests/proxy/test_rate_limit.py through actual dispatcher limiter and service adapter.
use ai_platform_gateway_foundation::{
    dispatcher_rate_limiter::*,
    protocol::{ServiceConfig, ServiceDefinition, ServiceRateLimitConfig},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
fn service(id: &str) -> ServiceRateLimits {
    ServiceRateLimits {
        service_id: id.into(),
        ..Default::default()
    }
}
fn config(global: i64) -> RateLimitConfig {
    RateLimitConfig {
        global_limit: Some(RateLimit::new(global, 60)),
        ..Default::default()
    }
}
#[test]
fn rate_limit_configuration_defaults_and_all_dimensions() {
    let defaults = RateLimitConfig::default();
    assert!(defaults.global_limit.is_none());
    assert!(defaults.tenant_limits.is_empty());
    assert!(defaults.user_limits.is_empty());
    assert!(defaults.service_limits.is_empty());
    assert!(defaults.ip_limits.is_none());
    let default = RateLimit::new(100, 60);
    assert_eq!(default.burst, 0);
    assert_eq!(default.strategy, "sliding_window");
    let limit = RateLimit {
        burst: 10,
        ..default
    };
    assert_eq!((limit.requests, limit.window, limit.burst), (100, 60, 10));
    assert_eq!(limit.strategy, "sliding_window");
    let c = RateLimitConfig {
        global_limit: Some(RateLimit::new(1000, 60)),
        tenant_limits: BTreeMap::from([
            ("tenant_001".into(), RateLimit::new(500, 60)),
            ("tenant_002".into(), RateLimit::new(200, 60)),
        ]),
        user_limits: BTreeMap::from([("user_001".into(), RateLimit::new(100, 60))]),
        service_limits: BTreeMap::from([("langgraph".into(), RateLimit::new(50, 60))]),
        ip_limits: Some(RateLimit::new(10, 60)),
    };
    assert_eq!(c.global_limit.unwrap().requests, 1000);
    assert_eq!(c.tenant_limits["tenant_001"].requests, 500);
    assert_eq!(c.user_limits["user_001"].requests, 100);
    assert_eq!(c.service_limits["langgraph"].requests, 50);
    assert_eq!(c.ip_limits.unwrap().requests, 10);
}
#[tokio::test]
async fn first_request_and_exact_limit_then_rejection() {
    let limiter = RateLimiter::new(config(10));
    for _ in 0..10 {
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap();
    }
    assert_eq!(
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap_err()
            .key,
        "global"
    );
}
#[tokio::test]
async fn tenant_limits_are_shared_between_users_but_not_other_tenants() {
    let limiter = RateLimiter::new(RateLimitConfig {
        tenant_limits: BTreeMap::from([
            ("tenant_001".into(), RateLimit::new(5, 60)),
            ("tenant_002".into(), RateLimit::new(10, 60)),
        ]),
        ..Default::default()
    });
    for user in ["user_a", "user_a", "user_a", "user_b", "user_b"] {
        limiter
            .enforce("tenant_001", user, &service("test_service"), None)
            .await
            .unwrap();
    }
    for user in ["user_a", "user_b"] {
        assert_eq!(
            limiter
                .enforce("tenant_001", user, &service("test_service"), None)
                .await
                .unwrap_err()
                .key,
            "tenant:tenant_001"
        );
    }
    limiter
        .enforce("tenant_002", "user_b", &service("test_service"), None)
        .await
        .unwrap();
    for _ in 0..20 {
        limiter
            .enforce("unknown_tenant", "user_c", &service("test_service"), None)
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn different_users_keep_their_configured_limits() {
    let limiter = RateLimiter::new(RateLimitConfig {
        user_limits: BTreeMap::from([
            ("user_001".into(), RateLimit::new(3, 60)),
            ("user_002".into(), RateLimit::new(5, 60)),
        ]),
        ..Default::default()
    });
    for (user, count) in [("user_001", 3), ("user_002", 5)] {
        for _ in 0..count {
            limiter
                .enforce("", user, &service("test_service"), None)
                .await
                .unwrap();
        }
        assert_eq!(
            limiter
                .enforce("", user, &service("test_service"), None)
                .await
                .unwrap_err()
                .key,
            format!("user:{user}")
        );
    }
}
#[tokio::test]
async fn different_services_and_ips_keep_separate_limits() {
    let limiter = RateLimiter::new(RateLimitConfig {
        service_limits: BTreeMap::from([
            ("langgraph".into(), RateLimit::new(5, 60)),
            ("openai".into(), RateLimit::new(10, 60)),
        ]),
        ..Default::default()
    });
    for _ in 0..5 {
        limiter
            .enforce("", "user_001", &service("langgraph"), None)
            .await
            .unwrap();
    }
    assert_eq!(
        limiter
            .enforce("", "user_001", &service("langgraph"), None)
            .await
            .unwrap_err()
            .key,
        "service:langgraph"
    );
    limiter
        .enforce("", "user_001", &service("openai"), None)
        .await
        .unwrap();
    let limiter = RateLimiter::new(RateLimitConfig {
        ip_limits: Some(RateLimit::new(3, 60)),
        ..Default::default()
    });
    for _ in 0..3 {
        limiter
            .enforce(
                "",
                "user_001",
                &service("test_service"),
                Some("192.168.1.1"),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        limiter
            .enforce(
                "",
                "user_001",
                &service("test_service"),
                Some("192.168.1.1")
            )
            .await
            .unwrap_err()
            .key,
        "ip:192.168.1.1"
    );
    limiter
        .enforce(
            "",
            "user_001",
            &service("test_service"),
            Some("192.168.1.2"),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn burst_allows_exactly_eight_then_rejects() {
    let limiter = RateLimiter::new(RateLimitConfig {
        global_limit: Some(RateLimit {
            burst: 3,
            ..RateLimit::new(5, 60)
        }),
        ..Default::default()
    });
    for _ in 0..8 {
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap();
    }
    assert!(
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .is_err()
    );
}
fn multi() -> RateLimiter {
    RateLimiter::new(RateLimitConfig {
        global_limit: Some(RateLimit::new(100, 60)),
        tenant_limits: BTreeMap::from([("tenant_001".into(), RateLimit::new(50, 60))]),
        user_limits: BTreeMap::from([("user_001".into(), RateLimit::new(10, 60))]),
        ..Default::default()
    })
}
#[tokio::test]
async fn strictest_and_all_dimensions_are_checked() {
    let limiter = multi();
    for _ in 0..10 {
        limiter
            .enforce("tenant_001", "user_001", &service("test_service"), None)
            .await
            .unwrap();
    }
    assert_eq!(
        limiter
            .enforce("tenant_001", "user_001", &service("test_service"), None)
            .await
            .unwrap_err()
            .key,
        "user:user_001"
    );
    let limiter = multi();
    for i in 0..50 {
        limiter
            .enforce(
                "tenant_001",
                &format!("user_{i:03}"),
                &service("test_service"),
                None,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        limiter
            .enforce("tenant_001", "user_050", &service("test_service"), None)
            .await
            .unwrap_err()
            .key,
        "tenant:tenant_001"
    );
}
#[tokio::test]
async fn windows_expire_and_cleanup_slides_at_original_intervals() {
    for sliding in [false, true] {
        let time = Arc::new(AtomicU64::new(0));
        let clock = time.clone();
        let limiter = RateLimiter::with_clock(
            RateLimitConfig {
                global_limit: Some(RateLimit::new(2, 1)),
                ..Default::default()
            },
            10000,
            Arc::new(move || clock.load(Ordering::SeqCst) as f64 / 1000.),
        );
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap();
        if sliding {
            time.store(600, Ordering::SeqCst);
        }
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap();
        time.store(if sliding { 1200 } else { 1100 }, Ordering::SeqCst);
        limiter
            .enforce("", "user_001", &service("test_service"), None)
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn concurrent_requests_are_safe_and_respect_exact_limit() {
    for limit in [10, 5] {
        let limiter = Arc::new(RateLimiter::new(config(limit)));
        let mut tasks = vec![];
        for _ in 0..10 {
            let limiter = limiter.clone();
            tasks.push(tokio::spawn(async move {
                limiter
                    .enforce("", "user_001", &service("test_service"), None)
                    .await
            }));
        }
        let mut successes = 0;
        let mut failures = 0;
        for task in tasks {
            if task.await.unwrap().is_ok() {
                successes += 1
            } else {
                failures += 1
            }
        }
        assert_eq!(successes, limit);
        assert_eq!(failures, 10 - limit);
    }
}
#[test]
fn errors_contain_exact_limit_key() {
    for key in ["user:user_001", "global", "tenant:tenant_001"] {
        let error = RateLimitExceeded { key: key.into() };
        assert_eq!(error.key, key);
        assert!(error.to_string().contains(key));
    }
}
#[tokio::test]
async fn service_legacy_dictionary_and_configured_limits_use_actual_adapter() {
    let legacy = ServiceDefinition {
        service_id: "test_service".into(),
        rate_limit: Some(json!({"requests":3,"window":60,"burst":0,"strategy":"sliding_window"})),
        ..Default::default()
    };
    let configured = ServiceDefinition {
        service_id: "test_service_2".into(),
        service_config: Some(ServiceConfig {
            rate_limit: ServiceRateLimitConfig {
                enabled: true,
                requests: 2,
                window: 60,
                burst: 0,
                strategy: "sliding_window".into(),
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    for (definition, count) in [(legacy, 3), (configured, 2)] {
        let limiter = RateLimiter::new(RateLimitConfig::default());
        let service = definition.rate_limits_view().unwrap();
        for _ in 0..count {
            limiter
                .enforce("", "user_001", &service, None)
                .await
                .unwrap();
        }
        assert!(
            limiter
                .enforce("", "user_001", &service, None)
                .await
                .is_err()
        );
    }
}
