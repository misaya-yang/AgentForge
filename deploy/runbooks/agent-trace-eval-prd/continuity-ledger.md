# Agent Trace Eval PRD Harness Continuity Ledger

**Created:** 2026-06-26

**Harness Folder:** `deploy/runbooks/agent-trace-eval-prd`

---

## Purpose

Preserve cross-phase continuity between product intent, trace schema decisions, code facts, validation evidence, and next-phase handoff.

## Phase Continuity Chain

| Phase | Feature | Depends On | Unlocks | Handoff Boundary | Required Writeback |
| --- | --- | --- | --- | --- | --- |
| ATE-00 | ATE-F001 | none | ATE-01 | Baseline report proves contracts, current code facts, and validation commands. | Source packet facts, interface boundary ledger, progress log, handoff. |
| ATE-01 | ATE-F002 | ATE-00 | ATE-02 | API/schema report proves trace tables, tenant isolation, and score contract. | Final table/API names, permission decision, rollback notes, tests. |
| ATE-02 | ATE-F003 | ATE-01 | ATE-03 | Runtime report proves assistant trace capture works without SSE/session/latency regressions. | Trace writer interface, redaction rules, runtime event mapping, non-blocking persistence evidence, regression evidence. |
| ATE-03 | ATE-F004 | ATE-02 | ATE-04 | UI report proves Eval Assistant trace explorer and score workflow. | Route/nav/API client contracts, screenshots, browser findings, accessibility notes. |
| ATE-04 | ATE-F005 | ATE-03 | none | Terminal report proves whole-demand regression and next-wave readiness. | LangGraph/RAG handoff contracts, release blockers, validator evidence. |

## Interface Boundary Ledger

| Boundary | Current Fact | Source | Last Verified | Owner Phase |
| --- | --- | --- | --- | --- |
| Harness location | Root `docs/` is ignored; durable plan lives under `deploy/runbooks/agent-trace-eval-prd/`. | `.gitignore`, memory note, git check-ignore | 2026-06-26 | ATE-00 |
| Gateway assistant API | Public assistant traffic enters gateway `src/api/v1/assistant.py` and proxies to assistant-service. | Repo inspection | 2026-06-26 | ATE-00 |
| Assistant runtime | `agent_loop.py` emits `run_started`, terminal `run_finished` or `run_error`, and holds `run_id`, `request_id`, `session_id`, tenant and user context. | Repo inspection | 2026-06-26 | ATE-02 |
| Existing run table | `assistant_runs` stores run lifecycle but not nested spans or scores. | `database/migrations/034_assistant_gateway_foundation.sql` | 2026-06-26 | ATE-01 |
| Existing request traces | `request_traces` stores sampled request timing and trace steps; it is request-centric and dashboard-oriented. | `database/migrations/033_observability_and_quota_governance.sql` | 2026-06-26 | ATE-01 |
| Existing context metrics | `assistant_context_breakdown` stores context token breakdown keyed by request/run/session. | `database/migrations/039_assistant_context_metrics.sql` | 2026-06-26 | ATE-02 |
| Frontend route shell | `web/src/router.tsx` and `web/src/layouts/AppLayout.tsx` own protected routes and nav items. | Repo inspection | 2026-06-26 | ATE-03 |
| Existing trace UI pattern | `web/src/pages/dashboard/components/panels/RequestTracePanel.tsx` is a useful timeline/table pattern but should not be overwritten. | Repo inspection | 2026-06-26 | ATE-03 |
| Agent latency guardrail | Trace writes are outside the user-facing assistant critical path; slow or failing persistence must not delay first token, stream event order, final response, or run status updates. | User clarification, 2026-06-26 | 2026-06-26 | ATE-02 |
| Trace taxonomy | Current public family enum is `trace_family=assistant\|langgraph_proxy\|rag`; `source_kind` is a historical planning alias only. First wave uses `workflow_kind=ai_assistant_chat`. | Source packet | 2026-06-27 | ATE-01 |
| Privacy boundary | Persist redacted previews and metrics by default; raw content retention is outside first wave. | Source packet | 2026-06-26 | ATE-01 |
| ATE-00 completion | Baseline architecture passed strict validation, docs-ignore proof, placeholder scan, repo manifest proof, and independent critic review. | `reports/ate-00-baseline-trace-architecture-report.md`, `reports/ate-00-baseline-trace-architecture-critic.md` | 2026-06-26 | ATE-00 |
| ATE-01 schema/API completion | Eval trace schema and API passed lint, targeted pytest, migration scan, OpenAPI compatibility, strict harness validation, and independent critic review. | `reports/ate-01-ai-assistant-trace-schema-and-api-report.md`, `reports/ate-01-ai-assistant-trace-schema-and-api-critic.md` | 2026-06-26 | ATE-01 |
| Eval route namespace | Eval API is registered under `/api/v1/eval`; `trace_family=assistant\|langgraph_proxy\|rag` is accepted consistently by trace list/detail/score/export paths. | `src/api/v1/eval.py`, `src/api/router.py` | 2026-06-27 | ATE-01 |
| Eval permission decision | Eval endpoints use dedicated `console:eval:view` for read and `console:eval:run` for write/run. Do not regress to `console:usage:view`. | `src/api/v1/eval.py`, ATE-01 report | 2026-06-27 | ATE-01 |
| ATE-02 runtime capture completion | Assistant trace capture passed lint, focused pytest, latency guard tests, token scan, ATE-01 API compatibility, and independent critic review. | `reports/ate-02-assistant-trace-capture-report.md`, `reports/ate-02-assistant-trace-capture-critic.md` | 2026-06-26 | ATE-02 |
| Trace writer interface | `AssistantTraceWriter` exposes `start_trace`, `record_event`, `record_span`, `finish_trace`, and `drain`; public methods submit bounded background tasks and never await DB writes. | `apps/assistant-service/src/assistant_service/core/trace_writer.py` | 2026-06-26 | ATE-02 |
| Runtime event mapping | Lifecycle, context, streaming model, tool, and error events are mapped into ATE-01 spans/events; terminal event writes are idempotent through `(trace_id, sequence_no)`. | `trace_writer.py`, `agent_loop.py`, `assistant_service.py` | 2026-06-26 | ATE-02 |
| Non-stream run correlation | Non-stream chat now creates a run UUID for trace correlation and returns it in the existing `run_id` response field. | `assistant_service.py`, `tests/services/assistant/test_agent_trace_capture.py` | 2026-06-26 | ATE-02 |
| ATE-03 data input | Assistant trace roots, spans, events, status, usage, timing, run/session/request ids, and redacted previews are available for the Eval Assistant UI through ATE-01 API. | ATE-02 report, ATE-01 API contract | 2026-06-26 | ATE-03 |
| ATE-03 UI completion | Eval Console UI passed frontend lint/type, Playwright browser checks, Eval API regression, screenshots, and independent critic review. | `reports/ate-03-eval-console-ui-report.md`, `reports/ate-03-eval-console-ui-critic.md` | 2026-06-26 | ATE-03 |
| Eval frontend route | `/eval` is lazy-loaded through the authenticated app shell and protected by `console:eval:view`; users without permission redirect to `/403`. | `web/src/router.tsx`, `web/e2e/eval-trace.spec.ts` | 2026-06-27 | ATE-03 |
| Eval frontend API client | `web/src/api/eval.ts` reads assistant traces and submits scores through ATE-01 endpoints; browser score payloads do not include `tenant_id`. | `web/src/api/eval.ts`, `web/e2e/eval-trace.spec.ts` | 2026-06-26 | ATE-03 |
| Eval browser evidence | Desktop 1440x900 and mobile 390x844 Eval screenshots exist under `web/.playwright/`; e2e asserts no horizontal overflow and keyboard row selection. | `web/.playwright/eval-desktop.png`, `web/.playwright/eval-mobile.png` | 2026-06-26 | ATE-03 |
| Frontend command deviation | In this Codex runtime, root `pnpm -C web ...` resolves to pnpm 11.x and fails before scripts run; project-version validation from `web/` uses `pnpm@10.33.0` and passes. | ATE-03 report | 2026-06-26 | ATE-03 |
| ATE-04 terminal regression | Whole-demand pytest, frontend lint/type/e2e, example config validation, broad ruff, harness strict validation, ATE-04 completion gate, and full-demand completion gate passed. Latest backend regression is `33 passed, 2 skipped` after post-review trace hardening. | `reports/ate-04-release-regression-and-handoff-report.md` | 2026-06-26 | ATE-04 |
| Post-review trace hardening | Non-stream setup failures now finish failed traces, terminal span status is monotonic under delayed background writes, non-operator Eval access requires authenticated user scope, and multi-user/multi-turn trace regressions are covered. | `assistant_service.py`, `trace_writer.py`, `src/api/v1/eval.py`, `tests/services/assistant/test_agent_trace_capture.py`, `tests/api/test_eval_traces.py` | 2026-06-26 | ATE-04 |
| ATE-04 lint remediation | Required broad ruff over `apps/assistant-service/src/assistant_service` initially found 636 lint errors; scoped remediation cleared the gate. | ATE-04 report, lint remediation plan | 2026-06-26 | ATE-04 |
| LangGraph Proxy Trace contract | Family uses `trace_family=langgraph_proxy`, `workflow_kind=langgraph_agent_run`, transparent proxy metadata, bounded redacted payloads, and non-blocking persistence. | ATE-04 report, source packet | 2026-06-27 | ATE-04 |
| RAG Trace contract | Family uses `trace_family=rag`, retrieval/greeting workflow kinds, retrieval spans, document/citation metadata, bounded redacted excerpts, and non-blocking persistence. | ATE-04 report, source packet | 2026-06-27 | ATE-04 |

## Code Summary Writeback Rules

- ATE-01 must write final table names, migration filenames, API route path, schema types, and permission decision into this ledger.
- ATE-02 must write the trace writer interface, event-to-span mapping, redaction function, non-blocking latency guard evidence, and regression evidence into this ledger.
- ATE-03 must write final frontend route, nav key, API client types, visual states, and browser evidence into this ledger.
- ATE-04 must write the next-wave LangGraph proxy and RAG trace boundaries, including which fields are inherited and which dependencies remain blocked.

## Current Continuity Status

- Active phase: ATE-04
- Active feature-oracle item: ATE-F005
- Current decision: ATE-04 is the verified terminal phase; the first-wave AI Assistant trace Eval PRD is complete.
- Next action: No further phase remains in this harness. Start a future user-approved expansion for LangGraph Proxy Trace or RAG Trace when requested.
