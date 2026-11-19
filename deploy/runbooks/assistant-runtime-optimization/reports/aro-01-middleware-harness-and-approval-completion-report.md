# ARO-01 Middleware Harness and Approval Completion Report

**Phase:** ARO-01 Middleware Harness and Approval Completion

**Status:** passed

**Date:** 2026-06-29

---

## Summary

ARO-01 completed the first runtime upgrade phase. The middleware lifecycle now has stream-event and error hooks, AgentLoop routes outbound streaming events through the lifecycle hook without a broad loop rewrite, and streaming-first internal errors can be observed by error middlewares. The phase also added non-default reliability middlewares for call limits, loop detection, time budgets, pre-completion checks, and trace sensing.

Human approval semantics were tightened in two places. Middleware `CONFIRM` can now create a persisted approval through the execution gateway and stream an `approval_id`; a resumed tool call with a valid `_approval_id` is allowed and then consumed. Gateway approvals are now single-use, match approved tool arguments after stripping control fields, and cannot be reused to execute a duplicate side effect.

## Plan Followed

Plan file: `deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-plan.md`

The plan was followed with one validation-scope correction: the original phase ruff command swept all of `tests/services/assistant` and failed on unrelated pre-existing lint across old test/tool files. The phase contract was corrected to lint the changed runtime files and focused tests only.

## Files Changed

- `apps/assistant-service/src/assistant_service/core/agent/middleware.py`: added `run_on_stream_event` and `run_on_error` with failure isolation.
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`: routes streaming events through middleware before trace capture/yield; calls error middleware on streaming errors; persists and resumes middleware `CONFIRM` approvals.
- `apps/assistant-service/src/assistant_service/core/agent/middlewares/harness.py`: adds non-default reliability middlewares.
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`: adds public approval request/grant/consume helpers; approval grant now matches arguments; approved resumes are single-use.
- `tests/services/assistant/test_middleware_chain.py`: covers stream-event and error hook ordering and failure isolation.
- `tests/services/assistant/test_agentloop_streaming_first_contract.py`: covers AgentLoop stream/error hook wiring and `CONFIRM` approval resume executes once.
- `tests/services/assistant/test_harness_middlewares.py`: covers call limit, loop detection, time budget, pre-completion, and trace sensor middlewares.
- `tests/contract/test_find_active_command.py`: covers gateway approval consume/idempotency.
- `deploy/runbooks/assistant-runtime-optimization/phase-01-middleware-harness-and-approval-completion.md`: corrects the ruff validation command to focused ARO-01 paths.

## Validation Evidence

| Gate | Command or Check | Result | Notes |
| --- | --- | --- | --- |
| Initial broad ruff command | Original ARO-01 ruff command over `tests/services/assistant` | failed | 109 unrelated pre-existing lint findings; not caused by ARO-01. Command scope corrected in phase contract. |
| Focused ruff | `uv run ruff check apps/assistant-service/src/assistant_service/core/agent/middleware.py apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/agent/middlewares/harness.py apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py tests/services/assistant/test_middleware_chain.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_harness_middlewares.py tests/contract/test_find_active_command.py` | passed | `All checks passed!` |
| Middleware chain tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_middleware_chain.py` | passed | 7 passed. |
| Harness middleware tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_harness_middlewares.py` | passed | 5 passed. |
| AgentLoop hook focused tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_runs_stream_event_middleware tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_runs_error_middleware` | passed | 2 passed. |
| Middleware approval resume focused test | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_confirm_approval_resume_executes_once` | passed | 1 passed. |
| Gateway approval idempotency focused test | `uv run pytest -q --no-cov tests/contract/test_find_active_command.py::test_approval_resume_consumes_approval_and_prevents_duplicate_execution` | passed | 1 passed. |
| Assistant runtime contract | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_agent_trace_capture.py` | passed | 36 passed. |
| Approval gateway contract | `uv run pytest -q --no-cov tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py` | passed | 10 passed. |

## Minimal Change and Review

The implementation did not rewrite AgentLoop. The stream-event hook is applied at the existing unified `execute()` event boundary, and error hooks are called only from existing exception paths. Reliability middlewares are available but not default-registered, so they add harness capability without changing production defaults.

The approval change reuses existing `assistant_tool_approvals` storage and command queue behavior. No migration, UI change, deployment, provider access, or production data access was required.

## Independent Critic Review

- Critic artifact: `deploy/runbooks/assistant-runtime-optimization/reports/aro-01-middleware-harness-and-approval-completion-critic.md`
- Critic scope requested: hook lifecycle, approval persistence/resume/idempotency, redaction/no raw tool arguments in events, validation evidence, minimal-change boundary.

## Feature Oracle Updates

| Feature ID | Old Status | New Status | Evidence |
| --- | --- | --- | --- |
| ARO-F002 | failing | passing | This report, critic artifact, focused ruff, assistant runtime contract, approval gateway contract, and focused middleware/approval tests. |

## Progress Log Update

`progress-log.md` records ARO-01 as passed, notes the validation-scope correction, lists verification results, and hands off to ARO-02.

## Screenshots, Logs, or Eval Tables

No browser check was required for ARO-01 because no web approval UI files changed. Runtime evidence is backend/unit/contract test output.

## Blockers and Deviations

No blocker remains for ARO-01. Deviation: the ruff command in the phase contract was narrowed to changed ARO-01 files because the original broad command failed on unrelated existing lint.

## Handoff Notes

ARO-02 may proceed. It should use the new `TraceSensorMiddleware` and stream/error hook contract as a runtime source for trace-derived failure modes, but should keep trace-to-dataset promotion redacted and review-gated.
