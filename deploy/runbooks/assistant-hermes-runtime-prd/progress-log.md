# Assistant Hermes OpenClaw Runtime PRD Progress Log

**Created:** 2026-06-30

**Adapted On Main:** 2026-07-01

**Harness Folder:** `deploy/runbooks/assistant-hermes-runtime-prd`

---

## Current State

- Status: AHR-05 operating model and release gate passed; all six feature-oracle items (AHR-F001 through AHR-F006) are passing. The full requirement chain AHR-00 through AHR-05 is complete.
- Active phase: AHR-05
- Active feature-oracle item: AHR-F006
- Clean-state note: docs/runbook files were created on `origin/dev_06-30` and adapted into current `main`; AHR-01, AHR-02, AHR-03, and AHR-04 changed only Assistant runtime/memory/tool-safety/trace/eval/UI code, tests, and harness docs. No production data, deployment, migration, schema change, Hermes code, OpenClaw code, or external service was changed.

## Session Log

| Date | Agent Role | Phase | Summary | Evidence | Next Step |
| --- | --- | --- | --- | --- | --- |
| 2026-06-30 | planner | AHR-00 | Created Assistant vs Hermes product PRD, source packet, feature oracle, manifest, continuity ledger, and phase chain. | `deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md`, `deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md` | Execute AHR-00, re-check source paths, and write the baseline report before implementation phases proceed. |
| 2026-06-30 | planner | AHR-00 | Expanded the PRD harness with OpenClaw runtime protocol mechanisms: context compiler, bootstrap evidence, layered memory, transcript separation, pre-compaction flush, skills/plugins governance, prompt hook policy, canonical approval plan, and read-only doctor/status. | `deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md`, `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json`, `deploy/runbooks/assistant-hermes-runtime-prd/phase-manifest.md` | Validate harness, then execute AHR-00 with a three-source source matrix and terminology invariants. |
| 2026-07-01 | planner | AHR-00 | Adapted the PRD harness into current `main`, corrected local AI--Platfform/OpenClaw paths, changed Hermes evidence to the local technical report, added PM framing, MVP slice, and success metrics. | `product-prd.md`, `source-packet.md`, local path probe output | Run strict harness validation, then execute AHR-00 before any runtime implementation. |
| 2026-07-01 | actor/critic | AHR-00 | Passed comparative baseline evidence. Rechecked AI--Platfform anchors, local Hermes report, official Hermes remote source anchors, and OpenClaw local paths; corrected stale OpenClaw path drift and recorded terminology invariants, implementation boundaries, validation command map, actor report, and critic verdict. | `reports/ahr-00-comparative-baseline-evidence-report.md`, `reports/ahr-00-comparative-baseline-evidence-critic.md`, `source-packet.md`, `continuity-ledger.md` | Execute AHR-01 / AHR-F002: additive Assistant run/session/turn envelope and context snapshot contract. |
| 2026-07-01 | actor/critic | AHR-01 | Implemented additive `assistant-turn-contract/v1` context snapshots and terminal envelopes across AgentLoop streaming events, non-stream `AssistantService.chat()`, SSE `done`, and trace metadata. Added tests for streaming success, approval pending, non-stream parity, and trace metadata. | `reports/ahr-01-entry-session-and-turn-contract-report.md`, `reports/ahr-01-entry-session-and-turn-contract-critic.md`, `apps/assistant-service/src/assistant_service/core/turn_contract.py`, `tests/services/assistant/test_agentloop_streaming_first_contract.py`, `tests/services/assistant/test_agent_trace_capture.py` | Execute AHR-02 / AHR-F003: memory lifecycle, transcript separation, and completed-turn sync discipline using the AHR-01 contract. |
| 2026-07-01 | actor/critic | AHR-02 | Implemented additive `assistant-memory-lifecycle/v1` memory lifecycle helpers, bounded/deduped source-store writes, profile/workspace source separation, completed-turn-only durable memory sync, runtime memory provenance, pre-compaction flush evidence, and compaction lineage. | `reports/ahr-02-memory-context-and-compaction-lineage-report.md`, `reports/ahr-02-memory-context-and-compaction-lineage-critic.md`, `apps/assistant-service/src/assistant_service/core/runtime/memory/lifecycle.py`, `tests/services/assistant/test_memory_manager.py`, `tests/services/assistant/tools/test_context_tools.py` | Execute AHR-03 / AHR-F004: fail-closed risky tool execution, approval binding, sandbox policy, audit redaction, and skill/plugin/tool governance. |
| 2026-07-01 | actor/critic | AHR-03 | Implemented fail-closed risky tool execution through ToolRegistry, duplicate-registration denial by default, gateway-approved invocation metadata, approval DB-failure denial, recursive audit summary redaction, MCP catalog/parameter sanitization, and configured sandbox runtime enforcement for code execution. | `reports/ahr-03-tool-permission-and-runtime-safety-report.md`, `reports/ahr-03-tool-permission-and-runtime-safety-critic.md`, `tests/services/assistant/tools/test_tool_runtime_safety.py`, `apps/assistant-service/src/assistant_service/core/tools/tool_registry.py`, `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py` | Execute AHR-04 / AHR-F005: connect runtime safety, memory, context, trace, eval, golden, review, and cockpit surfaces. |
| 2026-07-01 | actor/critic | AHR-04 | Implemented bounded runtime trajectory trace metadata, Eval dashboard runtime health, trace-to-golden expected trajectory metadata, runtime feedback cases, 16-case golden regression, and Eval UI runtime cockpit/detail surfaces. Browser regression covered desktop, Thread View, dark zh-CN, mobile, RAG, LangGraph, KB RAGAS, export, dataset, score, and no-horizontal-overflow paths. | `reports/ahr-04-observability-eval-and-regression-cockpit-report.md`, `reports/ahr-04-observability-eval-and-regression-cockpit-critic.md`, `apps/assistant-service/src/assistant_service/core/trace_writer.py`, `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py`, `web/src/pages/eval/index.tsx`, `web/src/pages/eval/components/AssistantTraceDetail.tsx` | Execute AHR-05 / AHR-F006: operating model, read-only runtime doctor/status, no-go thresholds, waiver policy, and whole-demand release gate. |
| 2026-07-01 | actor/critic | AHR-05 | Published Assistant Runtime Operating Model runbook, implemented offline `make verify-assistant-runtime-dev` regression gate (5/5 groups, 162 tests + 16 golden cases), JSON/Markdown reports under `reports/assistant-runtime-regression/`, CI adoption policy (local/offline stage), waiver policy, no-go thresholds, terminology invariants, and whole-demand regression across AHR-F001 through AHR-F006. | `reports/ahr-05-operating-model-and-release-gate-report.md`, `reports/ahr-05-operating-model-and-release-gate-critic.md`, `deploy/runbooks/assistant-runtime-operating-model.md`, `scripts/assistant_runtime_regression.py`, `Makefile` | No further phases. The requirement chain is complete. |

## Known Blockers

- Exact line-level code facts should be re-verified inside each implementation phase before code edits because AI--Platfform, Hermes remote source, and OpenClaw may change.
- No local Hermes checkout is recorded; AHR-00 verified official remote source anchors at HEAD `729bbb7a309a3d13d8cc7d1cd2fbab79e7d969f7`.
- OpenClaw docs/code were used as comparison evidence only; no OpenClaw runtime, doctor, or secret-touching command was run.
- AHR-01 did not add a dedicated max-iteration golden fixture; the terminal exit reason path exists, and AHR-02/AHR-04 should add deeper interrupted/max-iteration memory/eval cases.
- AHR-02 uses per-path process locks for local markdown memory writes; cross-process file locking remains out of scope and should be revisited only if multiple assistant-service workers share the same local file-backed memory directory.
- AHR-04 added Eval UI surfaces and passed the focused Playwright route check; screenshot evidence lives under `web/.playwright/`.
- Code execution now requires the configured sandbox runtime by default. Local or deployment environments without `runsc` must install it or explicitly configure the fallback path.
- AHR-04 dashboard runtime-health SQL aggregation was covered through repository/API tests and static gates, not against production data; AHR-05 should include it in read-only doctor/status coverage.

## Clean Exit Checklist

- Phase report written or blocker documented.
- Feature oracle updated only for worked items.
- Validation evidence linked.
- Next target phase and prompt are clear.
