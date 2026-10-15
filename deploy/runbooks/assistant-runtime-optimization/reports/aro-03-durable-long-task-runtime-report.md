# ARO-03 Durable Long Task Runtime Report

**Phase:** ARO-03 Durable Long Task Runtime

**Status:** passed

**Date:** 2026-06-29

---

## Summary

ARO-03 added additive assistant run checkpoints and a non-executing resume-preparation path. Checkpoints persist the latest safe phase, iteration, message-state hash, pending tool summary, approval ID, idempotency metadata, bounded resume payload, and status. They intentionally do not store raw prompts, raw message content, full tool arguments, credentials, or unbounded payloads.

The resume path is conservative: `/api/v1/assistant/runs/{run_id}/resume` validates tenant/user scope and the latest checkpoint, returns `ready` only when an approval checkpoint has a matching approved approval, and returns `blocked` with a checkpoint when resume cannot proceed. It does not replay the model turn or execute tools by itself, so duplicate side effects still go through the existing approval and command de-dupe gates.

## Plan Followed

Plan file: `deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-plan.md`

The implementation followed the plan with one validation-scope correction: the original broad ARO-03 ruff command swept old `database`, `tests/services/assistant`, and `tests/contract` files and failed on 108 unrelated pre-existing lint findings. The phase contract was narrowed to the ARO-03 changed runtime, gateway, route, and test files.

## Files Changed

- `database/migrations/067_assistant_run_checkpoints.sql`: additive checkpoint table and indexes.
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`: adds checkpoint save/fetch, sanitizer, DB-less fallback, and resume-preparation helpers.
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`: writes checkpoints at existing run/model/tool/approval/terminal boundaries without broad orchestration rewrite.
- `apps/assistant-service/src/assistant_service/core/assistant_service.py`: exposes `prepare_run_resume`.
- `apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py`: adds `/runs/{run_id}/resume` returning a non-executing resume plan.
- `tests/contract/test_find_active_command.py`: covers checkpoint sanitization, tenant/user scoping, approval-blocked resume, and approved resume readiness.
- `tests/services/assistant/test_agentloop_streaming_first_contract.py`: covers AgentLoop checkpoint writes and prompt-secret non-persistence.
- `tests/contract/test_migrated_routes_equivalence.py`: extends route delegation coverage to resume and applies mechanical lint fixes required by focused ruff.
- `deploy/runbooks/assistant-runtime-optimization/phase-03-durable-long-task-runtime.md`: narrows the ruff command to changed ARO-03 files.

## Validation Evidence

| Gate | Command or Check | Result | Notes |
| --- | --- | --- | --- |
| Initial broad ruff command | Original ARO-03 ruff command over `database`, full assistant agent/gateway dirs, all assistant tests, and all contract tests | failed | 108 unrelated pre-existing lint findings. Command scope corrected to changed ARO-03 paths. |
| Focused ARO-03 ruff | `uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py apps/assistant-service/src/assistant_service/core/assistant_service.py apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/contract/test_find_active_command.py tests/contract/test_migrated_routes_equivalence.py` | passed | `All checks passed!` |
| Focused new tests | `uv run pytest -q --no-cov tests/contract/test_find_active_command.py::test_run_checkpoint_sanitizes_payload_and_fetches_latest tests/contract/test_find_active_command.py::test_prepare_run_resume_blocks_without_required_approval tests/contract/test_find_active_command.py::test_prepare_run_resume_ready_after_approved_checkpoint tests/contract/test_migrated_routes_equivalence.py::test_as_run_approval_routes_exist_and_delegate_to_assistant_service tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_persists_checkpoints_without_prompt_text` | passed | 5 passed. |
| Checkpoint runtime tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | passed | 37 passed. |
| Run-state contract tests | `uv run pytest -q --no-cov tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py tests/api/test_assistant_sessions.py tests/contract/test_migrated_routes_equivalence.py::test_as_run_approval_routes_exist_and_delegate_to_assistant_service` | passed | 20 passed. |
| Migration review | `database/migrations/067_assistant_run_checkpoints.sql` inspected | passed | Additive `CREATE TABLE IF NOT EXISTS` plus indexes; no destructive SQL. |

## Minimal Change and Review

No external workflow engine was introduced. AgentLoop was not reorganized; checkpoint writes sit at existing event boundaries. The resume route only validates and returns checkpoint state. Tool execution continues through the existing execution gateway, approval status checks, and command de-dupe path.

Checkpoint payloads are bounded by the gateway sanitizer. Message state is represented by a digest/hash, pending tools store `tool_id`, `tool_name`, and argument hash, and sensitive keys/strings are redacted before persistence.

## Independent Critic Review

- Critic artifact: `deploy/runbooks/assistant-runtime-optimization/reports/aro-03-durable-long-task-runtime-critic.md`
- Critic scope requested: checkpoint schema, gateway sanitizer, AgentLoop write points, resume route, approval/command idempotency, tenant/user filters, and validation evidence.

## Feature Oracle Updates

| Feature ID | Old Status | New Status | Evidence |
| --- | --- | --- | --- |
| ARO-F004 | failing | passing | This report, critic artifact, focused ruff, focused checkpoint tests, 37 assistant runtime tests, and 20 run-state contract tests. |

## Progress Log Update

`progress-log.md` records ARO-03 as passed, notes the broad ruff correction, and hands off cache/context/performance telemetry to ARO-04.

## Screenshots, Logs, or Eval Tables

No browser evidence was required for ARO-03. Durable evidence is backend test output plus the additive migration file.

## Blockers and Deviations

No blocker remains for ARO-03. Deviation: the ruff command was narrowed because the original broad command failed on unrelated existing lint outside the ARO-03 edit boundary.

## Handoff Notes

ARO-04 may proceed. It should use checkpoint status and ARO-02 failure modes as telemetry dimensions when measuring context/cache/reasoning optimization regressions.
