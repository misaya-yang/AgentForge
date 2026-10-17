use super::*;
use std::collections::BTreeSet;
fn id(provider: &str) -> Identity {
    Identity {
        provider: provider.into(),
        model: "same-model".into(),
        version: "v1".into(),
    }
}
fn policy() -> Policy {
    Policy {
        schema: SCHEMA.into(),
        tenant: "tenant-a".into(),
        policy_id: "chat".into(),
        version: "1".into(),
        cost_currency: "USD".into(),
        strategy: Strategy::Priority,
        candidates: vec![
            CandidateRule {
                target: id("a"),
                priority: 1,
                weight: 1,
            },
            CandidateRule {
                target: id("b"),
                priority: 2,
                weight: 3,
            },
        ],
        required_capabilities: BTreeSet::from(["tools".into()]),
        max_latency_ms: None,
        max_cost_nanos: None,
        metric_max_age_ms: 1000,
        max_attempts: 2,
    }
}
fn ctx(p: &Policy) -> Evaluation {
    Evaluation {
        tenant: p.tenant.clone(),
        actor: "alice".into(),
        invocation_id: "request1".into(),
        policy_pin: p.pin().unwrap(),
        seed: "eval-seed-v1".into(),
        at_ms: 2000,
        required_capabilities: BTreeSet::new(),
    }
}
fn candidate(provider: &str) -> Candidate {
    Candidate {
        tenant: "tenant-a".into(),
        target: id(provider),
        authorized: true,
        invocation_compatible: true,
        catalog_available: true,
        capabilities: BTreeSet::from(["tools".into()]),
        latency: Some(Observation {
            value: 100,
            measured_at_ms: 1500,
            reference: "measured-row".into(),
        }),
        cost: Some(Observation {
            value: 20,
            measured_at_ms: 2000,
            reference: "immutable-price".into(),
        }),
    }
}
#[test]
fn stable_pin_id_and_snapshot_order() {
    let mut p = policy();
    p.strategy = Strategy::Weighted;
    let a = evaluate(&p, ctx(&p), vec![candidate("a"), candidate("b")]).unwrap();
    let b = evaluate(&p, ctx(&p), vec![candidate("b"), candidate("a")]).unwrap();
    assert_eq!(a, b);
    a.verify().unwrap();
    let mut tampered = a.clone();
    tampered.selected = id("evil");
    assert_eq!(tampered.verify(), Err(RoutingError::PinMismatch));
}
#[test]
fn explicit_auth_and_tenant_boundaries() {
    let p = policy();
    let mut a = candidate("a");
    a.authorized = false;
    let d = evaluate(&p, ctx(&p), vec![a, candidate("b")]).unwrap();
    assert_eq!(d.selected, id("b"));
    assert!(d.trace[0].exclusions.contains(&Exclusion::NotAuthorized));
    let mut foreign = candidate("a");
    foreign.tenant = "tenant-b".into();
    assert_eq!(
        evaluate(&p, ctx(&p), vec![foreign]),
        Err(RoutingError::InvalidSnapshot)
    );
    let mut context = ctx(&p);
    context.tenant = "tenant-b".into();
    assert_eq!(evaluate(&p, context, vec![]), Err(RoutingError::Forbidden));
}
#[test]
fn same_name_provider_and_version_do_not_collide() {
    let mut p = policy();
    p.candidates[0].target.version = "v2".into();
    let d = evaluate(&p, ctx(&p), vec![candidate("a"), candidate("b")]).unwrap();
    assert_eq!(d.selected, id("b"));
    assert!(
        d.trace
            .iter()
            .any(|t| t.target.version == "v2"
                && t.exclusions.contains(&Exclusion::CatalogUnavailable))
    );
}
#[test]
fn costs_unknown_stale_and_exact_boundary() {
    let mut p = policy();
    p.strategy = Strategy::LowestCost;
    p.max_cost_nanos = Some(20);
    let mut a = candidate("a");
    a.cost = None;
    let d = evaluate(&p, ctx(&p), vec![a, candidate("b")]).unwrap();
    assert_eq!(d.selected, id("b"));
    let mut b = candidate("b");
    b.cost.as_mut().unwrap().measured_at_ms = 999;
    assert_eq!(
        evaluate(&p, ctx(&p), vec![b]),
        Err(RoutingError::NoCandidate)
    );
    let mut b = candidate("b");
    b.cost.as_mut().unwrap().value = 21;
    assert_eq!(
        evaluate(&p, ctx(&p), vec![b]),
        Err(RoutingError::NoCandidate)
    );
}
#[test]
fn future_and_missing_latency_is_not_zero() {
    let mut p = policy();
    p.strategy = Strategy::LowestLatency;
    let mut a = candidate("a");
    a.latency.as_mut().unwrap().measured_at_ms = 2001;
    let mut b = candidate("b");
    b.latency = None;
    assert_eq!(
        evaluate(&p, ctx(&p), vec![a, b]),
        Err(RoutingError::NoCandidate)
    );
}
#[test]
fn current_capability_filter_cannot_be_overridden() {
    let p = policy();
    let mut a = candidate("a");
    a.capabilities.clear();
    let d = evaluate(&p, ctx(&p), vec![a, candidate("b")]).unwrap();
    assert_eq!(d.selected, id("b"));
    let mut c = ctx(&p);
    c.required_capabilities.insert("vision".into());
    assert_eq!(
        evaluate(&p, c, vec![candidate("a"), candidate("b")]),
        Err(RoutingError::NoCandidate)
    );
}
#[test]
fn policy_validation_and_pin_fail_closed() {
    let mut p = policy();
    let old = ctx(&p);
    p.candidates[0].weight = 2;
    assert_eq!(evaluate(&p, old, vec![]), Err(RoutingError::PinMismatch));
    p.candidates[0].weight = 0;
    assert_eq!(p.validate(), Err(RoutingError::InvalidPolicy));
    let mut p = policy();
    p.candidates.push(p.candidates[0].clone());
    assert!(p.validate().is_err());
    let mut p = policy();
    p.max_cost_nanos = Some(MAX_EXACT + 1);
    assert!(p.validate().is_err());
}
#[test]
fn unknown_execution_never_resends() {
    let p = policy();
    let d = evaluate(&p, ctx(&p), vec![candidate("a"), candidate("b")]).unwrap();
    for outcome in [
        ProviderOutcome::Unknown,
        ProviderOutcome::ExecutionMayHaveStarted,
        ProviderOutcome::OutputStarted,
        ProviderOutcome::Completed,
    ] {
        assert!(!permits_next_attempt(outcome, 1, &d));
    }
    assert!(permits_next_attempt(
        ProviderOutcome::DefinitelyNotSent,
        1,
        &d
    ));
    assert!(!permits_next_attempt(
        ProviderOutcome::DefinitelyNotSent,
        0,
        &d
    ));
    assert!(!permits_next_attempt(
        ProviderOutcome::DefinitelyNotSent,
        2,
        &d
    ));
}
#[test]
fn duplicate_snapshot_is_rejected() {
    let p = policy();
    assert_eq!(
        evaluate(&p, ctx(&p), vec![candidate("a"), candidate("a")]),
        Err(RoutingError::InvalidSnapshot)
    );
}

#[test]
fn evaluation_seed_does_not_depend_on_random_request_ids() {
    let mut p = policy();
    p.strategy = Strategy::Weighted;
    let a = evaluate(&p, ctx(&p), vec![candidate("a"), candidate("b")]).unwrap();
    let mut context = ctx(&p);
    context.invocation_id = "different-request".into();
    let b = evaluate(&p, context, vec![candidate("a"), candidate("b")]).unwrap();
    assert_eq!(a.selected, b.selected);
    assert_ne!(a.id, b.id);
}
