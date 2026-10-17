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
