# Phase 04 - Shadow and conditionally adopt a Rust model projector

- PHASE_ID: PPR-04
- FEATURE_ID: PPR-F005
- DEPENDS_ON: PPR-02

## Outcome

Rust projects exactly the same provider frames as Python without issuing a second provider request, and becomes owner only if protocol/accounting parity and normalized resource/capacity gates pass.

## Scope

In:

- Offline replay of saved, redacted provider frames plus a live tee in which one provider request feeds both projectors and only the current owner emits to the runtime.
- Success, error, cancellation, usage, provider-omits-usage and backpressure parity.
- Warmed incremental RSS per active stream, peak RSS, CPU, local p99 and supported streams at the same container budget.
- Strangler cutover and rollback behind the ADR-approved owner switch.

Out:

- Duplicate provider requests, changed randomness, changed lease/snapshot/accounting semantics or latency-only justification.
- “Close enough” event projection.

## Done when

- [ ] Fixtures and live-tee corpus are byte-identical across success and failure paths with exactly one provider request per turn.
- [ ] Usage and omitted-usage fallback are identical.
- [ ] Rust reduces warmed incremental RSS per stream by at least 60%, supports at least 1.5 times the streams in the same budget, and does not regress CPU or local p99.
- [ ] If any adoption gate misses, Python remains owner and the phase records measured-not-adopted.
- [ ] If adopted, cutover, rollback, independent review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Fixture projection | Extended `make sdk-sse-contract` | Byte identity including errors/cancellation |
| One-call shadow | Provider request counter plus tee receipt | No duplicate cost or changed randomness |
| Accounting | Usage-present and usage-omitted fixtures | Billing semantics match |
| Resource curve | Warmed 1/10/25/50-stream profiles | Incremental memory and capacity gate |
| Rollback | Owner switch round trip | Python remains a safe fallback |

## Stop or confirm

- Ask before live provider shadowing, traffic cutover or changing deployed images.
- Stop on any lease, accounting, cancellation or public event divergence.
- Required review: independent protocol, accounting, performance and security review.
