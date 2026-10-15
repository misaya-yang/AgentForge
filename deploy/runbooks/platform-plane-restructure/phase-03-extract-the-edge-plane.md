# Phase 03 - Conditionally extract Edge

- PHASE_ID: PPR-03
- FEATURE_ID: PPR-F004
- DEPENDS_ON: PPR-02

## Outcome

Edge becomes an independent deployment unit only when the approved ADR shows that gateway replica/worker scaling or Governance-only isolation cannot satisfy the measured capacity, latency or failure-isolation gate. Otherwise the phase closes as measured-not-adopted with the current deployment retained.

## Scope

In:

- Reproduce the PPR-00 problem and compare the simpler T0/T1 alternatives before implementation.
- If adopted, move the existing Edge implementation without changing auth, admission, rate-limit, routing or quota semantics.
- Implement only the ADR-approved authenticated handoff, timeout, retry, backpressure, error and SSE ownership contract.
- Multi-worker capacity and rate-limit fault tests under the named load profile.

Out:

- Rewriting already-correct Lua admission/rate limiting, changing policy, or choosing Rust in this phase.
- Dormant service code when the split is not adopted.

## Done when

- [ ] Pre-implementation evidence shows why T0/T1 fails the precommitted gate, or the split is recorded as measured-not-adopted.
- [ ] If adopted, admission never oversells, each limiter dimension counts once, identity cannot be forged/replayed, and dependency failures fail closed.
- [ ] Edge added p99 stays inside the PPR-00 local budget and the measured capacity/isolation benefit exceeds the precommitted adoption threshold.
- [ ] Public contracts and live product behavior are unchanged.
- [ ] Rollback, security review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Alternative comparison | Same load against T0/T1 and candidate split | New service is necessary |
| Admission/rate fuzz | Concurrent multi-worker failure and replay suite | No oversell or double count |
| Trust boundary | Forged, stale, duplicate and unavailable-upstream cases | Fail-closed authenticated handoff |
| Product path | Live SSE/API/browser receipts | No contract or ownership drift |

## Stop or confirm

- Ask before publishing a port, changing Compose topology or cutting traffic.
- Stop if extraction needs auth/quota semantics or a public contract change.
- A measured-not-adopted result is valid and must remove experimental default-path code.
- Required review: independent architecture, auth/quota and security review.
