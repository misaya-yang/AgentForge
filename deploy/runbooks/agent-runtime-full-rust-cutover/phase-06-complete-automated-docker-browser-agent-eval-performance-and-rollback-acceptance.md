# Phase 06 - Complete automated, Docker, browser, Agent Eval, performance, and rollback acceptance

- PHASE_ID: FRC-06
- FEATURE_ID: FRC-F007
- DEPENDS_ON: FRC-05

## Outcome

The deletion-state release passes every automated and live gate, stays within performance/resource budgets, and survives a complete old-new-old-new rollback rehearsal.

## Scope

In:

- Rust/Python/Web/SDK tests, fresh/upgrade/idempotent migrations, two reproducible images, Docker health, full product browser matrix, three live Eval cohorts, fault injection, performance/resource measurements, rollback and final Git integration.

Out:

- Production deployment or destructive cleanup of Docker volumes/images/caches.
- New product features unrelated to compatibility or migration correctness.

## Done when

- [ ] All automated gates pass with zero unexplained skip/error; both OCI digests match their source/SBOM lock.
- [ ] Three eight-scenario live cohorts each score at least 6/8, median at least 7/8, with zero infrastructure error and unchanged evaluator.
- [ ] TTFT, tool concurrency, queue bound, RSS and total Compose memory meet the approved thresholds.
- [ ] New release, frozen rollback bundle, and new release again recover compatible sessions without schema downgrade, duplicate billing or side effects.
- [ ] Final merge leaves local/remote `main` identical, clean, with one worktree and no migration branch.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Automated | `make agent-runtime-release-gate` | Rust, Gateway, Knowledge, Web, SDK, migration, isolation, RAG and harness gates all pass. |
| Runtime | `make doctor && make validate-config && make validate && make status` | Repository-owned final Compose is healthy. |
| Live quality | Three real-provider Agent Eval cohorts plus complete browser/fault matrix | General Agent quality and product compatibility are preserved. |
| Rollback | `make agent-runtime-rollback-rehearsal` | Additive data survives old-new-old-new deployment without duplicate effects. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Required final review must approve security, correctness, product quality, resource evidence and rollback before merge/push; production release remains outside this harness.
