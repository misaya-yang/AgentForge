# ARO-04 Performance Cost and Context Optimization Report

**Phase:** ARO-04 Performance Cost and Context Optimization

**Status:** passed

**Date:** 2026-06-29

---

## Summary

ARO-04 made performance/cost/context optimization measurable without enabling risky adaptive routing. The assistant runtime now emits trace-safe prompt-prefix identity, tool-schema identity, estimated context utilization, and preserves provider cache token metrics through model usage aggregation and trace/run metadata. Tool selection now has deterministic tie-break ordering for equal score/tier cases.

No live provider load test, production config change, schema migration, deployment, adaptive model routing, or embedding-based selector rollout was performed. No latency or cost-savings claim is made; the phase adds evidence surfaces and quality gates.

## Plan Followed

Plan file: `deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-plan.md`

The implementation followed the plan with one validation-scope correction: the original broad `ruff-context-cost` command still sweeps unrelated pre-existing lint across old `core/tools` and assistant test files. The phase contract was corrected to the changed ARO-04 paths after recording the broad failure.

## Files Changed

- `apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py`: adds stable hash helpers, provider cache usage normalization, trace-safe cache usage payloads, and cache/context telemetry builder.
- `apps/assistant-service/src/assistant_service/core/models/model_registry.py`: preserves normalized OpenAI/DashScope nested cache tokens, Gemini `cachedContentTokenCount`, and Anthropic cache read/write tokens in usage.
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`: emits prompt-prefix/tool-schema hashes and context utilization on `context_budget`; normalizes usage before aggregation so cache/reasoning integer metrics survive.
- `apps/assistant-service/src/assistant_service/core/tools/tool_selector.py`: adds deterministic tie-break ordering by tier and tool name while preserving relevance-first selection.
- `tests/services/assistant/test_model_registry.py`: covers provider cache usage normalization.
- `tests/services/assistant/test_agentloop_streaming_first_contract.py`: covers context-budget hash/utilization telemetry and cache usage preservation without prompt leakage.
- `tests/services/assistant/test_tool_selector.py`: covers deterministic selector ordering.
- `deploy/runbooks/assistant-runtime-optimization/phase-04-performance-cost-and-context-optimization.md`: narrows the ruff command to changed ARO-04 paths after broad pre-existing lint failure.

## Validation Evidence

| Gate | Command or Check | Result | Notes |
| --- | --- | --- | --- |
| Initial broad ruff | `uv run ruff check apps/assistant-service/src/assistant_service/core/rag apps/assistant-service/src/assistant_service/core/quality apps/assistant-service/src/assistant_service/core/models apps/assistant-service/src/assistant_service/core/tools tests/services/assistant tests/services/eval/test_golden_regression_gate.py` | failed | 107 unrelated pre-existing lint findings in untouched `core/tools/*` and old assistant tests. |
| Focused ARO-04 ruff | `uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py apps/assistant-service/src/assistant_service/core/models/model_registry.py apps/assistant-service/src/assistant_service/core/tools/tool_selector.py tests/services/assistant/test_agentloop_streaming_first_contract.py tests/services/assistant/test_model_registry.py tests/services/assistant/test_tool_selector.py tests/services/eval/test_golden_regression_gate.py` | passed | `All checks passed!` |
| Focused cache/context tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_model_registry.py::TestSanitizeUsage tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_emits_context_budget_without_prompt_text tests/services/assistant/test_agentloop_streaming_first_contract.py::test_streaming_first_usage_preserves_provider_cache_metrics tests/services/assistant/test_tool_selector.py` | passed | 8 passed. |
| Assistant context tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/tools/test_context_tools.py` | passed | 29 passed. |
| Eval quality gate | `uv run pytest -q --no-cov tests/services/eval/test_golden_regression_gate.py tests/services/eval/test_evaluator_executor.py` | passed | 26 passed. |
| Diff hygiene | `git diff --check` | passed | No whitespace errors. |

## Minimal Change and Review

The change stays inside the ARO-04 runtime boundary. It reuses existing context budget events, usage payloads, trace writer behavior, and tool selector logic instead of adding a new observability service or routing engine. Adaptive routing and embedding tool selection remain unimplemented/disabled because no eval evidence proves quality improvement over the existing safe path.

Telemetry is bounded and trace-safe: persisted payloads contain hashes, counts, estimated token utilization, and normalized integer usage metrics, not raw prompts, user messages, tool schema JSON, credentials, or provider secrets.

## Independent Critic Review

- Critic artifact: `deploy/runbooks/assistant-runtime-optimization/reports/aro-04-performance-cost-and-context-optimization-critic.md`
- Critic scope requested: actor report, changed ARO-04 files, validation evidence, prompt/cache telemetry redaction, deterministic tool order, quality gates, and no unsupported cost-savings claim.

## Feature Oracle Updates

| Feature ID | Old Status | New Status | Evidence |
| --- | --- | --- | --- |
| ARO-F005 | failing | passing | This report, critic artifact, focused ARO-04 ruff, focused cache/context tests, assistant context tests, and eval quality gate. |

## Progress Log Update

`progress-log.md` records ARO-04 as passed, notes the broad ruff correction, and hands off whole-demand release regression and operating model work to ARO-05.

## Screenshots, Logs, or Eval Tables

No browser or live provider evidence was required. Cache-token evidence is fixture-based and trace/run-payload observable through normalized usage fields. TTFT remained covered by the existing `ttft` event and assistant trace tests.

## Blockers and Deviations

No ARO-04 blocker remains. Deviation: the broad ruff command was narrowed after it failed on unrelated pre-existing lint outside the ARO-04 edit boundary.

## Handoff Notes

ARO-05 may proceed. It should run whole-demand regression across ARO-F001 through ARO-F005, publish operating docs/SLO no-go thresholds, and keep this phase's no-cost-claim boundary unless live measured evidence is supplied.
