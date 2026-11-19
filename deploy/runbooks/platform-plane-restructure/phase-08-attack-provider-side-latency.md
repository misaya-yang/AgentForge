# Phase 08 - Evaluate provider-side latency without confounding

- PHASE_ID: PPR-08
- FEATURE_ID: PPR-F009
- DEPENDS_ON: PPR-00

## Outcome

Model, variant, cache and thinking-policy alternatives are compared with randomized interleaved trials, uncertainty, quality, failure and cost evidence; any default or product-target change remains an explicit owner decision.

## Scope

In:

- Pilot runs estimate variance and determine a pre-reviewed sample plan.
- Randomized interleaved blocks at matched concurrency, prompt/config digest and time window.
- Median, IQR, confidence interval, failures, cached-input rate, cost and answer quality for every candidate.
- At least 30 successful samples per candidate for exploration; any p95 or release claim requires at least 100 successful samples per candidate unless the reviewed precision plan requires more.

Out:

- Turning thinking off solely to win latency, keyword routing, semantic cache on default chat, or changing defaults without approval.
- Calling end-to-end TTFT a provider-only measurement.

## Done when

- [ ] Raw assignment order and trials prove candidates were interleaved rather than compared across different days.
- [ ] Claimed deltas exceed uncertainty and include quality, failure, cache and cost evidence.
- [ ] Local timing components remain inside PPR-00 gates.
- [ ] A candidate meets the existing target without quality loss, or the experiment records an evidenced negative conclusion and proposes—but does not silently apply—a new target.
- [ ] Product/Eval review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Assignment | Seeded randomized block receipt | Time drift is not treatment assignment |
| Statistics | Raw-trial bootstrap/precision report | Claims reflect uncertainty |
| Cache/cost | Provider usage receipts, redacted | Win is economically explained |
| Quality | Same representative cohort for every candidate | No latency-for-quality trade |
| Local guard | PPR-00 timing components | Provider experiment did not regress local path |

## Stop or confirm

- Ask before paid experiments, reading live credentials, changing a default model/variant/thinking policy or changing the product ceiling.
- Stop if candidates cannot be randomized under comparable provider conditions.
- Required review: independent product, Eval and performance-method review.
