# ARO-01 Middleware Harness and Approval Completion Plan

**Phase:** ARO-01 Middleware Harness and Approval Completion

**Feature:** ARO-F002

**Date:** 2026-06-29

## Plan

1. Inspect current middleware, AgentLoop streaming event/error paths, execution gateway approval primitives, approval routes, and existing focused assistant/contract tests.
2. Add lifecycle middleware hooks with failure isolation:
   - `run_on_stream_event(ctx, event)` that can observe/replace/drop stream events;
   - `run_on_error(ctx, error, phase)` that can emit diagnostic events without crashing the turn.
3. Wire the hooks into streaming-first AgentLoop event emission and error paths with minimal helper methods instead of a broad loop rewrite.
4. Add or extend focused middleware tests proving event ordering, event mutation/drop behavior, and middleware exception isolation.
5. Inspect CONFIRM and execution-gateway approval paths, then implement the smallest persisted approval/resume improvement that can be proven without migrations or UI scope expansion.
6. Run ARO-01 required validation:
   - `uv run ruff check apps/assistant-service/src/assistant_service/core/agent apps/assistant-service/src/assistant_service/core/gateway tests/services/assistant tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py`
   - `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_agent_trace_capture.py`
   - `uv run pytest -q --no-cov tests/contract/test_adr004_no_silent_regression.py tests/contract/test_find_active_command.py`
7. Write the ARO-01 actor report, critic artifact, oracle evidence, progress log, source-packet facts, continuity-ledger notes, and next handoff.

## Minimal-Change Boundary

Stay inside the ARO-01 contract paths. Do not introduce a new workflow engine, schema migration, provider dependency, or frontend approval UX unless the current approval path proves impossible to validate without that expansion.

## Review Focus

Completion must prove middleware hook failure isolation and approval/resume behavior. If persisted approval resume cannot be completed within existing schemas, document the exact blocker instead of pretending CONFIRM is complete.
