use super::*;
const PARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
fn headers() -> Headers {
    vec![
        ("traceparent".into(), PARENT.into()),
        (
            "tracestate".into(),
            "vendor=value,tenant@system=opaque".into(),
        ),
    ]
}
fn millis(v: u64) -> Duration {
    Duration::from_millis(v)
}
fn budget(now: Instant) -> Budget {
    Budget::admit(millis(300_000), 400_000, 100_000, now).unwrap()
}
#[test]
fn gateway_runtime_worker_knowledge_round_trip_fixture() {
    let now = Instant::now();
    let mut wire = headers();
    // The adapter supplies only an installed active carrier. Scope/signature
    // headers are created by their existing authorities, not Trace/Budget.
    wire.push(("baggage".into(), "tenant=private,prompt=secret".into()));
    let trace = Trace::from_admitted_carrier(&wire);
    let root = Budget::public_ingress(
        millis(300_000),
        400_000,
        100_000,
        now,
        &[(BUDGET_HEADER.into(), "999999999".into())],
    )
    .unwrap();
    let mut remaining = 300_000;
    let mut current = root.clone();
    for (index, edge) in [
        Edge::GatewayRuntimeCommand,
        Edge::RuntimeWorker,
        Edge::WorkerKnowledge,
    ]
    .into_iter()
    .enumerate()
    {
        let at = now + millis((index as u64 + 1) * 100);
        let sender = current.clone();
        let child = sender.child(edge.policy(), at).unwrap();
        let mut outgoing = vec![
            ("x-ai-tenant-id".into(), "admitted-tenant".into()),
            (
                "x-ai-platform-internal-token".into(),
                "configured-token".into(),
            ),
        ];
        trace.append(&mut outgoing);
        child
            .append(&mut outgoing, 100_000 + (index as u64 + 1) * 100, at)
            .unwrap();
        // Simulate network delay, fresh receiver route/lease ceiling and receipt.
        let received = Budget::admit(
            millis(300_000),
            400_000,
            100_150 + (index as u64) * 100,
            at + millis(50),
        )
        .unwrap()
        .constrain_authenticated(&outgoing, 100_150 + (index as u64) * 100, at + millis(50))
        .unwrap();
        let received_trace = Trace::from_admitted_carrier(&outgoing);
        assert_eq!(received_trace.trace_id(), trace.trace_id());
        let r = received.remaining(at + millis(50)).unwrap().as_millis() as u64;
        assert!(r <= remaining);
        remaining = r;
        current = received;
        assert!(!outgoing.iter().any(|(k, _)| k == "baggage"));
        assert_eq!(
            one(&outgoing, "x-ai-tenant-id").unwrap(),
            Some("admitted-tenant")
        );
        assert_eq!(
            one(&outgoing, "x-ai-platform-internal-token").unwrap(),
            Some("configured-token")
        );
        // Trace itself can be detached/cloned without copying any auth or scope.
        let mut detached = vec![];
        received_trace.clone().append(&mut detached);
        assert_eq!(detached.len(), 2);
        assert!(!detached.iter().any(|(k, _)| k.starts_with("x-ai-")));
    }
}
#[test]
fn no_active_adapter_means_no_raw_incoming_trace_promotion() {
    let raw = headers();
    let active: Headers = vec![];
    let trace = Trace::from_admitted_carrier(&active);
    let mut out = raw;
    trace.append(&mut out);
    assert!(one(&out, "traceparent").unwrap().is_none());
}
#[test]
fn trace_parser_rejects_duplicates_zero_ids_and_baggage() {
    for parent in [
        "",
        "00-00000000000000000000000000000000-0123456789abcdef-01",
        "ff-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        "00-0123456789ABCDEF0123456789abcdef-0123456789abcdef-01",
        "00-0123456789abcdef0123456789abcdef-0123456789abcdef-ff",
    ] {
        assert!(!valid_parent(parent));
    }
    let mut repeated = headers();
    repeated.push(("TraceParent".into(), PARENT.into()));
    assert!(Trace::from_admitted_carrier(&repeated).trace_id().is_none());
    for state in [
        "a=b,a=c",
        "bad key=v",
        "a=has=equals",
        "a=",
        ",a=b",
        "a=b,",
        "1bad=value",
    ] {
        assert!(!valid_state(state), "{state}");
    }
    assert!(valid_state("vendor=value, \ttenant@system=opaque"));
    assert!(!valid_state(&format!("a={}", "x".repeat(257))));
    assert!(!valid_state(
        &(0..33)
            .map(|n| format!("a{n}=b"))
            .collect::<Vec<_>>()
            .join(",")
    ));
}
#[test]
fn untrusted_budget_never_extends_and_authenticated_budget_only_attenuates() {
    let now = Instant::now();
    let forged = vec![
        (DEADLINE_HEADER.into(), u64::MAX.to_string()),
        (BUDGET_HEADER.into(), "3600000".into()),
        (HOPS_HEADER.into(), "8".into()),
    ];
    let local = Budget::public_ingress(millis(30_000), 140_000, 100_000, now, &forged).unwrap();
    assert_eq!(local.remaining(now).unwrap(), millis(30_000));
    let limited = local
        .constrain_authenticated(&forged, 100_000, now + millis(100))
        .unwrap();
    assert_eq!(
        limited.remaining(now + millis(100)).unwrap(),
        millis(29_900)
    );
    let short = vec![
        (DEADLINE_HEADER.into(), "105000".into()),
        (BUDGET_HEADER.into(), "10000".into()),
        (HOPS_HEADER.into(), "3".into()),
    ];
    let limited = local.constrain_authenticated(&short, 100_000, now).unwrap();
    assert_eq!(limited.remaining(now).unwrap(), millis(5000));
    assert_eq!(limited.hops, 3);
}
#[test]
fn malformed_partial_duplicate_and_expired_budgets_fail_closed() {
    let now = Instant::now();
    let local = budget(now);
    for value in [
        "0",
        "01",
        "-1",
        "+1",
        " 1",
        "1 ",
        "1.0",
        "18446744073709551616",
    ] {
        assert!(number(value, u64::MAX).is_err());
    }
    assert_eq!(
        local
            .constrain_authenticated(&[(BUDGET_HEADER.into(), "1000".into())], 100_000, now)
            .unwrap_err(),
        Error::Malformed
    );
    let duplicate = vec![
        (BUDGET_HEADER.into(), "1000".into()),
        ("X-AI-Budget-MS".into(), "1000".into()),
    ];
    assert_eq!(
        local
            .constrain_authenticated(&duplicate, 100_000, now)
            .unwrap_err(),
        Error::Duplicate
    );
    assert_eq!(
        local.remaining(now + millis(300_000)).unwrap_err(),
        Error::Expired
    );
    assert_eq!(
        local
            .constrain_authenticated(&[], 100_000, now + millis(300_000))
            .unwrap_err(),
        Error::Expired
    );
}
#[test]
fn hops_and_wall_clock_rollback_cannot_renew_authority() {
    let now = Instant::now();
    let mut local = budget(now);
    for _ in 0..7 {
        local = local
            .child(Edge::GatewayRuntimeCommand.policy(), now)
            .unwrap();
    }
    assert_eq!(
        local
            .child(Edge::GatewayRuntimeCommand.policy(), now)
            .unwrap_err(),
        Error::HopLimit
    );
    let at = now + millis(20_000);
    let mut wire = vec![];
    local.append(&mut wire, 10_000, at).unwrap();
    assert_eq!(one(&wire, BUDGET_HEADER).unwrap(), Some("280000"));
    assert_eq!(one(&wire, DEADLINE_HEADER).unwrap(), Some("290000"));
    assert_eq!(local.remaining(at).unwrap(), millis(280_000));
}
#[test]
fn post_unknown_outcome_never_retries_or_becomes_success() {
    let now = Instant::now();
    let local = budget(now);
    for method in [Method::Post, Method::Put, Method::Patch, Method::Delete] {
        assert!(!retry_allowed(method, 0, &local, now));
    }
    assert!(retry_allowed(Method::Get, 0, &local, now));
    assert!(!retry_allowed(Method::Get, 1, &local, now));
    assert!(!retry_allowed(
        Method::Get,
        0,
        &local,
        now + millis(300_000)
    ));
    assert_eq!(
        classify_late(Effect::Write, false),
        LateResult::UnknownSideEffect
    );
    assert_eq!(
        classify_late(Effect::Unknown, false),
        LateResult::UnknownSideEffect
    );
    assert_eq!(
        classify_late(Effect::Read, false),
        LateResult::ReadUnavailable
    );
    assert_eq!(
        classify_late(Effect::Write, true),
        LateResult::ReconcileReceipt
    );
}
#[test]
fn sse_has_bounded_liveness_without_command_total_timeout() {
    let now = Instant::now();
    let local = budget(now);
    let policy = Edge::RuntimeEvents.policy();
    assert!(policy.is_streaming());
    assert_eq!(policy.connect_timeout(&local, now).unwrap(), millis(10000));
    assert_eq!(policy.header_timeout(&local, now).unwrap(), millis(30000));
    assert_eq!(policy.idle_timeout(&local, now).unwrap(), millis(45000));
    assert_eq!(
        policy.total_timeout(&local, now).unwrap_err(),
        Error::InvalidPolicy
    );
    let child = local.child(policy, now + millis(290_000)).unwrap();
    assert_eq!(
        child.remaining(now + millis(290_000)).unwrap(),
        millis(10000)
    );
    assert_eq!(
        policy.idle_timeout(&child, now + millis(290_000)).unwrap(),
        millis(10000)
    );
}
