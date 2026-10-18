use super::*;
#[tokio::test]
async fn detached_capture_preserves_private_budget_without_cross_request_leak() {
    let wall = wall_ms();
    let budget = Budget::admit(Duration::from_secs(2), wall + 2000, wall, Instant::now()).unwrap();
    let captured = scoped_with_budget(
        OutboundContext {
            request_id: "request-a".into(),
            trace_carrier: BTreeMap::new(),
        },
        Some(budget),
        async { (capture(), capture_budget()) },
    )
    .await;
    assert!(capture_budget().is_none());
    assert!(current().request_id.is_empty());
    let task = tokio::spawn(scoped_with_budget(captured.0, captured.1, async {
        assert_eq!(current().request_id, "request-a");
        assert!(
            command_budget(Duration::from_secs(300))
                .unwrap()
                .remaining(Instant::now())
                .unwrap()
                <= Duration::from_secs(2)
        );
    }));
    scoped(
        OutboundContext {
            request_id: "request-b".into(),
            trace_carrier: BTreeMap::new(),
        },
        async {
            assert!(capture_budget().is_none());
            assert_eq!(current().request_id, "request-b");
        },
    )
    .await;
    task.await.unwrap();
    assert!(capture_budget().is_none());
}
#[tokio::test]
async fn correlation_discards_duplicate_or_orphan_trace_and_never_promotes_existing_headers() {
    let active = OutboundContext {
        request_id: "request-a".into(),
        trace_carrier: BTreeMap::from([("tracestate".into(), "vendor=value".into())]),
    };
    let headers = scoped(active, async {
        internal_http_headers(
            BTreeMap::from([
                (
                    "traceparent".into(),
                    "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".into(),
                ),
                ("baggage".into(), "tenant=private".into()),
                ("authorization".into(), "existing-auth".into()),
            ]),
            None,
            None,
            None,
        )
        .unwrap()
    })
    .await;
    assert!(!headers.contains_key("traceparent"));
    assert!(!headers.contains_key("tracestate"));
    assert!(!headers.contains_key("baggage"));
    assert_eq!(headers["authorization"], "existing-auth");
}

#[tokio::test]
async fn detached_future_captures_object_scope_and_budget_before_spawn_polls_it() {
    use ai_platform_auth::cedar::resources::{self, ObjectSubject, PolicyScope};
    let scope = PolicyScope {
        request_id: "policy-a".into(),
        trace_id: "trace-a".into(),
        ..Default::default()
    };
    let future = resources::scoped(scope, async {
        resources::remember_subject(ObjectSubject {
            tenant_id: "t".into(),
            user_id: "u".into(),
            ..Default::default()
        });
        let now = wall_ms();
        let budget =
            Budget::admit(Duration::from_secs(2), now + 2000, now, Instant::now()).unwrap();
        scoped_with_budget(
            OutboundContext {
                request_id: "outbound-a".into(),
                ..Default::default()
            },
            Some(budget),
            async {
                scoped(
                    OutboundContext {
                        request_id: "detached-a".into(),
                        ..Default::default()
                    },
                    async {
                        assert_eq!(resources::capture().request_id, "policy-a");
                        assert_eq!(resources::current_subject().unwrap().user_id, "u");
                        assert!(capture_budget().is_some());
                        assert_eq!(current().request_id, "detached-a");
                    },
                )
            },
        )
        .await
    })
    .await;
    assert!(resources::current_subject().is_none());
    assert!(capture_budget().is_none());
    tokio::spawn(future).await.unwrap();
    assert!(resources::current_subject().is_none());
    assert!(current().request_id.is_empty());
}
