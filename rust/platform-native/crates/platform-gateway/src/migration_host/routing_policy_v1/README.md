# Explicit versioned routing v1 integration

This module is inert until its unnumbered routing-policy SQL proposal is measured
and activated by the native canonical authority. No Cargo dependency is added.
Primary adds `pub mod routing_policy` in foundation and `pub mod routing_policy_v1`
in migration_host. Mount `http::router(service, existing_admission_service)` within
the existing BFF/CSRF middleware; do not expose a second listener.

The actual native LLM adapter retains `RoutingAuthority::from_principal` from its
existing resolved AdmissionService principal. Never deserialize this authority.
Operator bindings select policy ID, evaluation pin (optional) and seed. Request
body/header metadata cannot override tenant, policy contents, allowed candidates,
seed or model choice. An authorized evaluation controller may explicitly pin an
existing same-tenant publication; ordinary traffic resolves the operator's active
binding. An unconfigured policy keeps the published exact-model flow unchanged;
a configured but unavailable policy fails closed.

Call sequence:

1. `service.active(&authority, policy_id, optional_eval_pin)` loads and verifies
   an immutable same-tenant policy publication.
2. `service.candidates(&authority, &policy, &resolved_provider_records,
   &QuoteWorkload { input_tokens, output_tokens }, at_ms)` reads existing model
   registry and current model/API-key ACL, applies effective capabilities and
   typed catalog/credential availability. Provider records are exclusively from
   the existing RuntimeProviderCredentials authority. Input tokens and enforced
   output limit come from existing server parsing/metering, not caller estimates.
3. `service.decide(&authority, &policy, server_seed, invocation_id, at_ms,
   required_capabilities, candidates)` verifies publication, deterministically
   selects, inserts an append-only decision and reads it back. Only a matching
   readback is returned. A different result for the same actor/invocation conflicts.
4. Select the returned exact provider/model through the existing LLM codec and
   preflight again. Carry `decision.id` as `x-route-decision` on every successful
   JSON/stream response and in existing usage metadata. Never forward incoming
   x-route-decision as authoritative. Persist before provider dispatch.

GET `/api/v1/routing/decisions/{id}` resolves the header to policy pin/version,
seed, snapshot pin, candidate exclusions, observed metrics and selected identity.
Reads require the original same-tenant actor or current routing manager. Policy
publication/activation requires current admin or routing:manage permission;
activation uses expected-pin CAS. Versions and decisions have no Gateway UPDATE
or DELETE privilege. The SQL adds no budget or catalog authority.

Policy schema is `agentforge.routing_policy/v1`; pins and decision IDs use existing
JCS v2, leaving old hashes untouched. Strategies are priority, bounded positive
integer weighted draw, lowest observed latency and lowest projected cost. Ties use
explicit priority then exact provider/model/version. Weighted selection uses the
SHA256 seed/pin draw modulo bounded total weight; it is deterministic
and is not cryptographic randomness or an exact statistical sampler. Cost is
explicitly USD nanos, derived from existing price-book v2 input/output component
quotes, without spending/reserving budget. Current immutable price-book revision
must match the policy's model version, including for priority selection. This
opt-in feature therefore requires active price-book v2 routes. Missing terms,
unknown latency or stale/future observations are never zero-valued evidence.
Latency is the latest successful measured usage for the exact provider/model and
frozen price-book model version; it is not an inferred health probe or percentile.

Existing admission, leases, capacity, provider health and frozen usage receipts
remain authoritative. `permits_next_attempt` accepts only definite-not-sent,
explicit pre-output rate-limit, or proven rejection before execution; unknown,
possible execution, output-started and completed results prohibit automatic resend.
Even an allowed outcome still needs fresh existing admission/quota enforcement.
This module dispatches nothing and does not add retries to native LLM handling.

Source regressions cover stable IDs under snapshot permutation, policy tamper,
tenant/provider/version collisions, ACL/capability exclusion, unknown/stale/future
metrics, exact cost thresholds, invalid weights/precision, duplicate records and
unknown-outcome resend refusal. Rustfmt parse and diff checks ran; Cargo, DB,
canonical measurement, HTTP fixtures and real provider acceptance did not run in
this worker. Those gates remain primary-owned; registration is not activation.

## Native LLM path now wired

`LlmApi::new` preserves the old exact-model flow. `new_with_routing` accepts an
additional `LlmRoutingBindings::new(Vec<LlmRouteBinding>)`; each file-only operator
entry specifies tenant_id, alias, policy_id, optional policy_pin and seed. Aliases
cannot contain `/` and are rejected if a live registry model has that exact ID.
No routing field from HTTP bodies or incoming headers is honored. `Actor` retains
the original verified Admission principal's typed routing authority. Each candidate
passes the original codec, then uses its actual patched request and enforced
output limit with the existing admission token estimate. Final selected catalog,
ACL, codec, service preflight and admission run again before dispatch.

For reproducible evaluation, the server combines the configured seed with SHA256
of the original request bytes. Random invocation IDs distinguish durable decisions
but never salt weighted selection. Identical input bytes and seed/pin with the same
eligible catalog select the same target. Each candidate has its own workload,
including model-specific enforced defaults and choice count. Final preflight
workload and frozen price evidence must still match the durable decision. Price
activation drift causes a conflict before dispatch; no silent repricing occurs.
Both sync and stream responses carry the persisted ID; provider-supplied IDs are
removed. Native usage metadata carries route_decision_id and routing_policy_pin.
No automatic retry or fallback was added.
