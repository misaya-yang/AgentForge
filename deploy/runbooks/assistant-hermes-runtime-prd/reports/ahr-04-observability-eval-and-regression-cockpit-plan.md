# AHR-04 Observability Eval And Regression Cockpit Plan

Status: planned

Feature oracle item: AHR-F005

## Scope

Implement the smallest AHR-04 slice that makes Assistant runtime quality visible and regression-testable through the existing trace/eval platform. This phase will not introduce an external observability dependency, schema migration, deployment, production data mutation, or Hermes/OpenClaw code import.

## Planned Changes

- Extend `AssistantTraceWriter` trace metadata with a bounded `runtime_trajectory` summary derived from existing terminal envelope, context snapshot, transcript locator, trace writer health counters, redaction state, and AHR-03 tool-safety metadata when present.
- Extend `AgentTraceRepository.get_dashboard()` to return a `runtime_health` section for Assistant/RAG/LangGraph coverage, pass rates, trajectory pass rate, critical failures, tool-safety failures, outbox failures, pending review/judge counts, and latest baseline/candidate run labels.
- Extend trace-to-dataset feedback so generated golden/review cases preserve bounded runtime trajectory expectations: terminal status, context snapshot id, memory/pre-compaction evidence keys, gateway/sandbox decisions, redaction requirements, and required span kinds.
- Add runtime-specific golden cases to `tests/fixtures/eval/golden/assistant_regression_v1.jsonl`: approval denial, approval argument mismatch, sandbox unavailable, interrupted memory skip, stop/resume, max iterations, and redaction/export coverage without raw secrets.
- Add a compact Eval Console runtime cockpit strip that consumes `dashboard.runtime_health` and distinguishes Assistant enabled state from partial/wired RAG and LangGraph state.
- Add focused tests for dashboard runtime health, trace feedback trajectory metadata, trace writer metadata, golden coverage, and frontend type coverage through the existing `make verify-eval-dev` gate.

## Validation

- Focused backend tests while iterating:
  - `uv run pytest -q --no-cov tests/api/test_eval_traces.py tests/services/eval/test_trace_feedback.py tests/services/eval/test_golden_regression_gate.py`
  - `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_trace_capture.py`
- Required phase gate:
  - `make verify-eval-dev`
- If UI changes are made:
  - `corepack pnpm@10.33.0 -C web lint`
  - `corepack pnpm@10.33.0 -C web type-check`

## Evidence To Record

- Actor report: `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-04-observability-eval-and-regression-cockpit-report.md`
- Independent critic artifact: `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-04-observability-eval-and-regression-cockpit-critic.md`
- Writeback targets: `source-packet.md`, `continuity-ledger.md`, `progress-log.md`, `agent-handoff.md`, `feature-oracle.json`, `loop-state.json`, and `next-window-prompt.md`.
