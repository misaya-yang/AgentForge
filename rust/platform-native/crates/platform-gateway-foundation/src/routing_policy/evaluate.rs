use super::model::*;
use std::collections::{BTreeMap, BTreeSet};
fn fresh(o: &Option<Observation>, ctx: &Evaluation, p: &Policy) -> bool {
    o.as_ref().is_some_and(|o| {
        o.value <= MAX_EXACT
            && text(&o.reference)
            && o.measured_at_ms >= 0
            && o.measured_at_ms <= ctx.at_ms
            && ctx.at_ms - o.measured_at_ms <= i64::from(p.metric_max_age_ms)
    })
}
pub fn evaluate(
    policy: &Policy,
    context: Evaluation,
    mut snapshot: Vec<Candidate>,
) -> Result<Decision> {
    policy.validate()?;
    if context.tenant != policy.tenant {
        return Err(RoutingError::Forbidden);
    }
    if context.policy_pin != policy.pin()? {
        return Err(RoutingError::PinMismatch);
    }
    if !text(&context.actor)
        || !text(&context.invocation_id)
        || !text(&context.seed)
        || context.at_ms < 0
        || context.at_ms as u64 > MAX_EXACT
        || context.required_capabilities.len() > 64
        || context.required_capabilities.iter().any(|s| !text(s))
        || snapshot.len() > 1024
    {
        return Err(RoutingError::InvalidSnapshot);
    }
    snapshot.sort_by(|a, b| a.target.cmp(&b.target));
    let mut unique = BTreeSet::new();
    for c in &snapshot {
        if [&c.latency, &c.cost]
            .iter()
            .filter_map(|v| v.as_ref())
            .any(|o| {
                o.value > MAX_EXACT
                    || o.measured_at_ms < 0
                    || o.measured_at_ms as u64 > MAX_EXACT
                    || !text(&o.reference)
            })
        {
            return Err(RoutingError::InvalidSnapshot);
        }
        if c.tenant != context.tenant
            || !valid_identity(&c.target)
            || !unique.insert(&c.target)
            || c.capabilities.len() > 64
            || c.capabilities.iter().any(|s| !text(s))
        {
            return Err(RoutingError::InvalidSnapshot);
        }
    }
    let snapshot_pin = hash("snapshot", &snapshot)?;
    let catalog: BTreeMap<_, _> = snapshot.iter().map(|c| (&c.target, c)).collect();
    let required: BTreeSet<_> = policy
        .required_capabilities
        .union(&context.required_capabilities)
        .collect();
    let mut trace = vec![];
    for rule in &policy.candidates {
        let mut exclusions = vec![];
        let c = catalog.get(&rule.target).copied();
        if c.is_none_or(|c| !c.invocation_compatible) {
            exclusions.push(Exclusion::InvocationUnsupported);
        }
        if c.is_none_or(|c| !c.authorized) {
            exclusions.push(Exclusion::NotAuthorized)
        }
        if c.is_none_or(|c| !c.catalog_available) {
            exclusions.push(Exclusion::CatalogUnavailable)
        }
        if c.is_none_or(|c| required.iter().any(|r| !c.capabilities.contains(*r))) {
            exclusions.push(Exclusion::CapabilityMismatch)
        }
        let latency = c.filter(|c| c.authorized).and_then(|c| c.latency.clone());
        let cost = c.filter(|c| c.authorized).and_then(|c| c.cost.clone());
        if policy.max_latency_ms.is_some() || policy.strategy == Strategy::LowestLatency {
            if !fresh(&latency, &context, policy) {
                exclusions.push(Exclusion::LatencyUnknown)
            } else if policy
                .max_latency_ms
                .is_some_and(|m| latency.as_ref().unwrap().value > m)
            {
                exclusions.push(Exclusion::LatencyLimit)
            }
        }
        if policy.max_cost_nanos.is_some() || policy.strategy == Strategy::LowestCost {
            if !fresh(&cost, &context, policy) {
                exclusions.push(Exclusion::CostUnknown)
            } else if policy
                .max_cost_nanos
                .is_some_and(|m| cost.as_ref().unwrap().value > m)
            {
                exclusions.push(Exclusion::CostLimit)
            }
        }
        trace.push(CandidateTrace {
            target: rule.target.clone(),
            exclusions,
            latency,
            cost,
            priority: rule.priority,
            weight: rule.weight,
        });
    }
    trace.sort_by(|a, b| a.target.cmp(&b.target));
    let mut ranked: Vec<_> = trace.iter().filter(|t| t.exclusions.is_empty()).collect();
    ranked.sort_by(|a, b| {
        match policy.strategy {
            Strategy::LowestLatency => a
                .latency
                .as_ref()
                .unwrap()
                .value
                .cmp(&b.latency.as_ref().unwrap().value),
            Strategy::LowestCost => a
                .cost
                .as_ref()
                .unwrap()
                .value
                .cmp(&b.cost.as_ref().unwrap().value),
            _ => a.priority.cmp(&b.priority),
        }
        .then_with(|| a.priority.cmp(&b.priority))
        .then_with(|| a.target.cmp(&b.target))
    });
    if ranked.is_empty() {
        return Err(RoutingError::NoCandidate);
    }
    if policy.strategy == Strategy::Weighted {
        // Bounded integer weighted draw. Policy pin and eval seed are explicit;
        // no process-random hashing or wall clock enters the selection.
        let sum: u64 = ranked.iter().map(|r| u64::from(r.weight)).sum();
        let entropy = hash("weighted-draw", &(&context.policy_pin, &context.seed))?;
        let mut draw = u64::from_str_radix(&entropy[..16], 16)
            .map_err(|_| RoutingError::InvalidSnapshot)?
            % sum;
        let mut index = 0;
        for (i, c) in ranked.iter().enumerate() {
            if draw < u64::from(c.weight) {
                index = i;
                break;
            }
            draw -= u64::from(c.weight)
        }
        ranked.swap(0, index);
    }
    let eligible = ranked.iter().map(|r| r.target.clone()).collect::<Vec<_>>();
    let mut d = Decision {
        schema: "agentforge.route_decision/v1".into(),
        id: String::new(),
        policy_id: policy.policy_id.clone(),
        policy_version: policy.version.clone(),
        context,
        snapshot_pin,
        selected: eligible[0].clone(),
        eligible,
        trace,
        max_attempts: policy.max_attempts,
    };
    d.id = hash("decision", &d)?;
    Ok(d)
}
/// Never infer retry safety from an unclassified HTTP/network error. Even an
/// allowed class still requires fresh existing admission/quota and caller policy.
pub fn permits_next_attempt(outcome: ProviderOutcome, attempts: u8, decision: &Decision) -> bool {
    attempts > 0
        && attempts < decision.max_attempts
        && usize::from(attempts) < decision.eligible.len()
        && matches!(
            outcome,
            ProviderOutcome::DefinitelyNotSent
                | ProviderOutcome::ExplicitRateLimitBeforeOutput
                | ProviderOutcome::RejectedBeforeExecution
        )
}
