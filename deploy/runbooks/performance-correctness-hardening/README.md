# Performance and Correctness Hardening

## Goal

Validate the findings in reports/code-review/perf-review-2026-08-16.md and
reports/code-review/global-parallel-review-2026-08-16.md against the current
working tree, fix confirmed reachable defects in risk order, and prove the
result with measured performance, focused regression, full gates, and the
current local Compose runtime.

## Non-goals

- Do not claim all 388 report entries are equivalent or can be closed in one change.
- Do not implement REJECTED findings or uncorrected wording superseded by either report's review section.
- Do not deploy, push, commit, mutate production data, or rotate credentials.
- Do not trade correctness, tenant isolation, accounting, or recoverability for benchmark wins.

## Evidence rules

1. Static estimates are hypotheses, never performance results.
2. A finding is implemented only after current-code reproduction or a direct contract test.
3. Each phase has its own rollback boundary and focused gate.
4. A skipped live test is reported as skipped; it is not a pass.
5. Full-suite success cannot hide a failed complex-task, load, security, or runtime gate.

## Phase map

| Phase | Scope | Exit contract |
| --- | --- | --- |
| PCH-00 | Baseline, deduplication, current-tree triage | Reproducible baselines and authoritative finding ledger |
| PCH-01 | Security, session/idempotency, Assistant state-machine correctness | Confirmed P0 authorization and stuck-state paths closed |
| PCH-02 | Assistant memory, trace, context, checkpoint, tools and MCP hot paths | Measured per-turn overhead reduced without eval regression |
| PCH-03 | Gateway quota, usage, admission, rate-limit and DB/Redis amplification | Accounting semantics correct and round trips bounded |
| PCH-04 | Web stream state/rendering/bundle and SDK stream contracts | Terminal first-wins, long-stream render bounded, initial bundles reduced |
| PCH-05 | Knowledge ingestion/retrieval, Local Node and infrastructure | Partial-index and unbounded-worker defects closed with service tests |
| PCH-06 | Full regression, container/live/load re-test and final report | All required gates pass; remaining blockers explicitly enumerated |
| PCH-07 | Stop-safe tool exchange and strong browser interruption acceptance | Cancelled tool calls stay provider-valid and the next turn never receives HTTP 400 |

The authoritative current phase is loop-state.json. Detailed ordering and
acceptance thresholds live in optimization-plan.md.
