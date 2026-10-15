# ARO-04 Performance Cost and Context Optimization Plan

**Phase:** ARO-04 Performance Cost and Context Optimization

**Feature:** ARO-F005

**Status:** planned

**Date:** 2026-06-29

## Assumption

This phase should make optimization evidence measurable and gateable first. It will not enable new adaptive model routing, live provider load tests, or cost-saving claims without measured eval evidence.

## Plan

1. Normalize provider cache usage metadata into bounded integer fields that can survive `StreamDelta`, AgentLoop aggregation, and trace storage.
2. Add stable prompt-prefix and tool-schema identity telemetry without storing raw prompt, tool schema JSON, user message, provider keys, or secrets.
3. Emit cache/context telemetry on the existing `context_budget` event and preserve cache token metrics in final usage payloads.
4. Make tool-selection ordering deterministic for equal score/tier/token cases while preserving the existing safe selector fallback and budget behavior.
5. Add focused tests for cache metric normalization, prompt-prefix telemetry redaction, and deterministic tool ordering.
6. Run the ARO-04 validation gates. If broad lint still sweeps unrelated old findings, record the failure and run a focused changed-path command as phase evidence.

## Likely Files

- `apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py`
- `apps/assistant-service/src/assistant_service/core/models/model_registry.py`
- `apps/assistant-service/src/assistant_service/core/tools/tool_selector.py`
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `tests/services/assistant/test_model_registry.py`
- `tests/services/assistant/test_agentloop_streaming_first_contract.py`
- `tests/services/assistant/test_tool_selector.py` or an existing nearby assistant test file

## Validation

Required phase commands:

| Gate | Command |
| --- | --- |
| ruff-context-cost | `uv run ruff check apps/assistant-service/src/assistant_service/core/rag apps/assistant-service/src/assistant_service/core/quality apps/assistant-service/src/assistant_service/core/models apps/assistant-service/src/assistant_service/core/tools tests/services/assistant tests/services/eval/test_golden_regression_gate.py` |
| assistant-context-tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/tools/test_context_tools.py` |
| eval-quality-gate | `uv run pytest -q --no-cov tests/services/eval/test_golden_regression_gate.py tests/services/eval/test_evaluator_executor.py` |

Focused regression tests to add/run:

- Cache metric normalization from OpenAI/DashScope nested `prompt_tokens_details.cached_tokens`.
- Gemini `cachedContentTokenCount` normalization.
- Context budget event includes hashes/cache/context utilization and redacts raw prompt/user text.
- Tool selector deterministic ordering when score/tier ties.

## Minimal-Change Boundary

No provider SDK dependency, schema migration, new pricing model, adaptive routing rollout, or production config change is planned. Existing AgentLoop message construction and tool execution behavior should remain intact except deterministic ordering for ties.

## Evidence to Write

- Actor report with changed files, validation commands, and explicit no-cost-claim note.
- Independent critic artifact checking no raw prompt leakage and no unsupported savings claim.
- `feature-oracle.json` update for ARO-F005 only.
- Targeted `progress-log.md`, `continuity-ledger.md`, `source-packet.md`, `agent-handoff.md`, and `next-window-prompt.md` updates for ARO-05.
