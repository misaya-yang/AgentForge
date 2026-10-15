# ARO-03 Critic Verdict

**Phase:** ARO-03 Durable Long Task Runtime

**Feature:** ARO-F004

**Critic:** fresh context independent reviewer over actor report, current diff, phase contract, and validation evidence

**Critic Verdict:** approved

**Actor Report Reviewed:** `deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-report.md`

**Date:** 2026-06-29

---

## Critic Inputs

- Phase contract: `deploy/runbooks/assistant-runtime-optimization/phase-03-durable-long-task-runtime.md`
- Feature oracle item: `ARO-F004`
- Actor report: `deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-report.md`
- Changed files or diff: additive checkpoint migration, execution gateway checkpoint/resume helpers, AgentLoop checkpoint writes, run resume route, and focused tests.
- Validation evidence: focused ARO-03 ruff, focused checkpoint tests, assistant runtime contract tests, run-state contract tests.
- Runtime/browser/eval evidence: backend runtime evidence; no browser evidence required by this phase.
- Minimal-change boundary: no Temporal/cloud workflow dependency, no destructive migration, no broad AgentLoop rewrite.
- Regression scope: run persistence, approval pause/resume, command de-dupe, trace terminal behavior, tenant/user scoping.

## Findings

Approved. ARO-03 adds a lightweight checkpoint contract without replacing the runtime or introducing a workflow engine. The schema is additive and idempotent. The checkpoint payload design is appropriately conservative: messages are represented by a bounded digest/hash, tool arguments are reduced to hashes, sensitive keys and secret-looking strings are redacted, and resume payloads are bounded.

The resume endpoint is deliberately non-executing, which is the safer first cut. It validates tenant/user scoped run access, examines the latest checkpoint, blocks missing or mismatched approvals, and returns `ready` only when the checkpoint and approval state are compatible. This avoids bypassing the existing execution gateway, command de-dupe, and single-use approval consumption path.

The validation-scope correction is acceptable. The original broad ruff command failed on unrelated pre-existing lint outside ARO-03's changed files. The corrected command covers the actual checkpoint/resume implementation and tests.

## Requirement Coverage

- R1 Checkpoint Contract: satisfied by `assistant_run_checkpoints`, `save_run_checkpoint`, message-state hashes, pending-tool summaries, approval IDs, idempotency metadata, and sanitizer tests.
- R2 Resume Behavior: satisfied by `prepare_run_resume` and `/runs/{run_id}/resume`, with tests for blocked missing approval and ready approved checkpoint. Duplicate side effects are not executed by the resume route.
- R3 Failure Semantics: satisfied by `resume_blocked` checkpoints and blocked run status when resume cannot proceed.

## Test and Regression Assessment

Passed checks inspected:

- Focused ARO-03 ruff: passed.
- Focused checkpoint/resume tests: 5 passed.
- Assistant runtime contract: 37 passed.
- Run-state contract: 20 passed.

Residual risk: actual long-running replay is not implemented in this phase. The route prepares safe resume state; the next runtime iteration can build a worker/replay loop on this contract.

## Minimal-Change Assessment

The implementation stays inside the phase boundary. It adds one migration, gateway helpers, a small route, and AgentLoop checkpoint writes at existing boundaries. It does not change public chat request/response shape or execute migrations locally.

## Whole-Demand Regression Assessment

Not required for ARO-03. Whole-demand regression remains reserved for ARO-05.

## Waiver Reason

Not applicable.
